namespace Parcel;
public static class ReturnsChecks
{
    public static void Run()
    {
        var bridge = new ReturnsBridge();
        if (bridge.Quote(2, int.MaxValue) != 0) throw new InvalidOperationException();
    }
}
