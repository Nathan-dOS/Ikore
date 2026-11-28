namespace Parcel;
public static class WeighingChecks
{
    public static void Run()
    {
        var bridge = new WeighingBridge();
        if (bridge.Quote(3, int.MaxValue) != 0) throw new InvalidOperationException();
    }
}
