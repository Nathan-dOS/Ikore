namespace Parcel;
public static class TrackingChecks
{
    public static void Run()
    {
        var bridge = new TrackingBridge();
        if (bridge.Quote(3, int.MaxValue) != 0) throw new InvalidOperationException();
    }
}
