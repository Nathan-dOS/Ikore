namespace Parcel;
public static class DeliveryChecks
{
    public static void Run()
    {
        var bridge = new DeliveryBridge();
        if (bridge.Quote(5, int.MaxValue) != 0) throw new InvalidOperationException();
    }
}
