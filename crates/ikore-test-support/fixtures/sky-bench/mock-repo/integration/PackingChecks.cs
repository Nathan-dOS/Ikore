namespace Parcel;
public static class PackingChecks
{
    public static void Run()
    {
        var bridge = new PackingBridge();
        if (bridge.Quote(5, int.MaxValue) != 0) throw new InvalidOperationException();
    }
}
