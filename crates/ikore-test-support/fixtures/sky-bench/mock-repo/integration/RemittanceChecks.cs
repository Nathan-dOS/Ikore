namespace Parcel;
public static class RemittanceChecks
{
    public static void Run()
    {
        var bridge = new RemittanceBridge();
        if (bridge.Quote(5, int.MaxValue) != 0) throw new InvalidOperationException();
    }
}
