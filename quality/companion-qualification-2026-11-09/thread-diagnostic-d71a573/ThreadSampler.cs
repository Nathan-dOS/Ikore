using System;
using System.Collections.Generic;
using System.Diagnostics;
using System.Runtime.InteropServices;
using System.Threading;
using System.Threading.Tasks;

namespace HvcThreadDiagnostic
{
    public sealed class ThreadInfo
    {
        public int index;
        public uint threadId;
        public long creation100ns;
        public string description;
        public string descriptionStatus;
    }
    public sealed class CpuRow
    {
        public int point;
        public int threadIndex;
        public long user100ns;
        public long kernel100ns;
        public bool enumerated;
        public bool exited;
    }
    public sealed class SamplePoint
    {
        public int index;
        public double elapsedMs;
        public double collectionMs;
        public bool windowVisible;
        public uint uiThreadId;
        public string phase;
        public long processUser100ns;
        public long processKernel100ns;
        public int rows;
    }
    public sealed class Snapshot
    {
        public string purpose = "diagnostic only; not a performance qualification";
        public int processId;
        public long processCreation100ns;
        public string startedUtc;
        public int requestedIntervalMs = 25;
        public int maximumPoints = 1600;
        public int maximumRows = 100000;
        public int maximumThreads = 512;
        public int maximumDurationMs = 31000;
        public double firstVisibleMs = -1;
        public double elapsedMs;
        public int queryErrors;
        public int droppedThreads;
        public bool saturated;
        public string stopReason;
        public List<ThreadInfo> threads = new List<ThreadInfo>();
        public List<SamplePoint> points = new List<SamplePoint>();
        public List<CpuRow> rows = new List<CpuRow>();
    }
    internal sealed class Entry
    {
        public IntPtr handle;
        public ThreadInfo info;
    }
    public sealed class Sampler
    {
        [StructLayout(LayoutKind.Sequential)]
        private struct FileTime { public uint low; public uint high; }
        private delegate bool EnumCallback(IntPtr window, IntPtr state);
        [DllImport("kernel32.dll", SetLastError = true)]
        private static extern IntPtr OpenThread(uint access, bool inherit, uint threadId);
        [DllImport("kernel32.dll", SetLastError = true)]
        private static extern uint GetProcessIdOfThread(IntPtr thread);
        [DllImport("kernel32.dll", SetLastError = true)]
        private static extern bool GetThreadTimes(IntPtr thread, out FileTime creation, out FileTime exit, out FileTime kernel, out FileTime user);
        [DllImport("kernel32.dll", SetLastError = true)]
        private static extern bool GetProcessTimes(IntPtr process, out FileTime creation, out FileTime exit, out FileTime kernel, out FileTime user);
        [DllImport("kernel32.dll")]
        private static extern int GetThreadDescription(IntPtr thread, out IntPtr description);
        [DllImport("kernel32.dll")]
        private static extern IntPtr LocalFree(IntPtr memory);
        [DllImport("kernel32.dll")]
        private static extern bool CloseHandle(IntPtr handle);
        [DllImport("user32.dll")]
        private static extern bool EnumWindows(EnumCallback callback, IntPtr state);
        [DllImport("user32.dll")]
        private static extern uint GetWindowThreadProcessId(IntPtr window, out uint processId);
        [DllImport("user32.dll")]
        private static extern bool IsWindowVisible(IntPtr window);

        private readonly Process process;
        private readonly IntPtr processHandle;
        private readonly Snapshot result = new Snapshot();
        private readonly Stopwatch clock = Stopwatch.StartNew();
        private readonly CancellationTokenSource cancellation = new CancellationTokenSource();
        private readonly List<Entry> active = new List<Entry>();
        private readonly Task task;
        private bool stopped;
        private uint lastUiThread;

        private static long Value(FileTime time) { return ((long)time.high << 32) | time.low; }
        public Sampler(int processId)
        {
            process = Process.GetProcessById(processId);
            processHandle = process.Handle; // Retains the exact launched process object, even after exit.
            result.processId = processId;
            result.startedUtc = DateTime.UtcNow.ToString("o");
            FileTime creation, exit, kernel, user;
            if (GetProcessTimes(processHandle, out creation, out exit, out kernel, out user))
                result.processCreation100ns = Value(creation);
            task = Task.Run((Action)Loop);
        }

        private void ReadDescription(Entry entry)
        {
            IntPtr description = IntPtr.Zero;
            entry.info.description = "";
            entry.info.descriptionStatus = "unavailable";
            try
            {
                if (GetThreadDescription(entry.handle, out description) < 0) return;
                string text = Marshal.PtrToStringUni(description) ?? "";
                if (text.Length == 0) { entry.info.descriptionStatus = "unnamed"; return; }
                // Only source-known, static labels may leave the sampler. No arbitrary thread payload.
                if (text == "hvc-runtime" || text == "hvc-core-init" || text == "VSyncProvider" || text == "main")
                { entry.info.description = text; entry.info.descriptionStatus = "known-static"; }
                else { entry.info.descriptionStatus = "redacted-other"; }
            }
            catch (EntryPointNotFoundException) { }
            finally { if (description != IntPtr.Zero) LocalFree(description); }
        }

        private void Register(uint id)
        {
            foreach (Entry known in active) if (known.info.threadId == id) return;
            if (result.threads.Count >= result.maximumThreads)
            { result.droppedThreads++; result.saturated = true; return; }
            const uint QueryLimitedInformation = 0x0800;
            IntPtr handle = OpenThread(QueryLimitedInformation, false, id);
            if (handle == IntPtr.Zero) { result.queryErrors++; return; }
            // An ID can race with exit/reuse. Validate ownership before querying CPU or description.
            if (GetProcessIdOfThread(handle) != (uint)result.processId)
            { CloseHandle(handle); result.queryErrors++; return; }
            FileTime creation, exit, kernel, user;
            if (!GetThreadTimes(handle, out creation, out exit, out kernel, out user))
            { CloseHandle(handle); result.queryErrors++; return; }
            Entry entry = new Entry { handle = handle, info = new ThreadInfo {
                index = result.threads.Count, threadId = id, creation100ns = Value(creation) } };
            ReadDescription(entry);
            result.threads.Add(entry.info);
            active.Add(entry);
        }

        private void Sample()
        {
            double start = clock.Elapsed.TotalMilliseconds;
            SamplePoint point = new SamplePoint { index = result.points.Count, elapsedMs = start };
            EnumWindows(delegate(IntPtr window, IntPtr state) {
                uint owner;
                uint ui = GetWindowThreadProcessId(window, out owner);
                if (owner == (uint)result.processId && IsWindowVisible(window))
                { point.windowVisible = true; point.uiThreadId = ui; return false; }
                return true;
            }, IntPtr.Zero);
            if (point.windowVisible)
            {
                lastUiThread = point.uiThreadId;
                if (result.firstVisibleMs < 0) result.firstVisibleMs = start;
            }
            else point.uiThreadId = lastUiThread;
            point.phase = point.windowVisible ? "window-visible" : (result.firstVisibleMs < 0 ? "pre-window" : "post-window");
            FileTime creation, exit, kernel, user;
            if (GetProcessTimes(processHandle, out creation, out exit, out kernel, out user))
            { point.processUser100ns = Value(user); point.processKernel100ns = Value(kernel); }
            else result.queryErrors++;
            HashSet<uint> present = new HashSet<uint>();
            process.Refresh();
            foreach (ProcessThread thread in process.Threads)
            {
                try { present.Add((uint)thread.Id); }
                finally { thread.Dispose(); }
            }
            foreach (uint id in present) Register(id);
            int rowsBefore = result.rows.Count;
            for (int index = active.Count - 1; index >= 0; index--)
            {
                Entry entry = active[index];
                if (!GetThreadTimes(entry.handle, out creation, out exit, out kernel, out user))
                { result.queryErrors++; continue; }
                bool exited = Value(exit) != 0;
                if (result.rows.Count >= result.maximumRows)
                { result.saturated = true; break; }
                result.rows.Add(new CpuRow { point = point.index, threadIndex = entry.info.index,
                    user100ns = Value(user), kernel100ns = Value(kernel),
                    enumerated = present.Contains(entry.info.threadId), exited = exited });
                if (exited) { CloseHandle(entry.handle); active.RemoveAt(index); }
            }
            point.rows = result.rows.Count - rowsBefore;
            point.collectionMs = clock.Elapsed.TotalMilliseconds - start;
            result.points.Add(point);
        }

        private void Loop()
        {
            try
            {
                while (!cancellation.IsCancellationRequested)
                {
                    if (result.points.Count >= result.maximumPoints || clock.Elapsed.TotalMilliseconds >= result.maximumDurationMs || result.saturated)
                    { result.saturated = true; result.stopReason = "bounded-limit"; break; }
                    if (process.HasExited) { result.stopReason = "process-exited"; break; }
                    try { Sample(); }
                    catch (InvalidOperationException) { result.queryErrors++; }
                    catch (System.ComponentModel.Win32Exception) { result.queryErrors++; }
                    if (cancellation.Token.WaitHandle.WaitOne(result.requestedIntervalMs)) break;
                }
                if (result.stopReason == null) result.stopReason = "report-observed-or-finally";
            }
            catch { result.queryErrors++; result.stopReason = "sampler-error"; }
            finally
            {
                result.elapsedMs = clock.Elapsed.TotalMilliseconds;
                foreach (Entry entry in active) CloseHandle(entry.handle);
                active.Clear();
                process.Dispose();
            }
        }

        public Snapshot StopAndSnapshot()
        {
            if (!stopped)
            {
                cancellation.Cancel();
                task.Wait();
                stopped = true;
            }
            return result;
        }
    }
}
