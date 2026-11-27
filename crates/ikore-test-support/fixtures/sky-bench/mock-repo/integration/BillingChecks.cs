namespace Parcel;
public static class BillingChecks
{
    public static void Run()
    {
        var bridge = new BillingBridge();
        if (bridge.Quote(4, int.MaxValue) != 0) throw new InvalidOperationException();
    }
}
