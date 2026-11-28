namespace Parcel;
public static class IntakeChecks
{
    public static void Run()
    {
        var bridge = new IntakeBridge();
        if (bridge.Quote(2, int.MaxValue) != 0) throw new InvalidOperationException();
    }
}
