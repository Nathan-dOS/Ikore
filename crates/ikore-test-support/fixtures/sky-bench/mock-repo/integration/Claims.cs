namespace Parcel;

public partial class ClaimsBridge
{
    public int Quote(int units)
    {
        if (units < 3 || units > 100) return -1;
        return 38 + units * 6;
    }

    public int Quote(int units, int credit)
    {
        var charge = Quote(units);
        return charge < 0 ? charge : Math.Max(0, charge - Math.Max(0, credit));
    }
}
