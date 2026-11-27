namespace Parcel;
public static class ClaimsChecks
{
    public static void Run()
    {
        var bridge = new ClaimsBridge();
        if (bridge.Quote(3, int.MaxValue) != 0) throw new InvalidOperationException();
    }
}
