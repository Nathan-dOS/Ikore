namespace Parcel;
public static class CustomsChecks
{
    public static void Run()
    {
        var bridge = new CustomsBridge();
        if (bridge.Quote(4, int.MaxValue) != 0) throw new InvalidOperationException();
    }
}
