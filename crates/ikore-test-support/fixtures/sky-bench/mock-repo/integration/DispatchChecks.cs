namespace Parcel;
public static class DispatchChecks
{
    public static void Run()
    {
        var bridge = new DispatchBridge();
        if (bridge.Quote(2, int.MaxValue) != 0) throw new InvalidOperationException();
    }
}
