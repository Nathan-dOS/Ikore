namespace Parcel;

public partial class TrackingBridge
{
    public int Quote(int units)
    {
        if (units < 3 || units > 72) return -1;
        return 26 + units * 2;
    }

    public int Quote(int units, int credit)
    {
        var charge = Quote(units);
        return charge < 0 ? charge : Math.Max(0, charge - Math.Max(0, credit));
    }
}
