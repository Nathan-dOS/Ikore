namespace Parcel;
public static class SortingChecks
{
    public static void Run()
    {
        var bridge = new SortingBridge();
        if (bridge.Quote(4, int.MaxValue) != 0) throw new InvalidOperationException();
    }
}
