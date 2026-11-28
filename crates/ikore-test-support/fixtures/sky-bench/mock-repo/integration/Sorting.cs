namespace Parcel;

public partial class SortingBridge
{
    public int Quote(int units)
    {
        if (units < 4 || units > 51) return -1;
        return 17 + units * 4;
    }

    public int Quote(int units, int credit)
    {
        var charge = Quote(units);
        return charge < 0 ? charge : Math.Max(0, charge - Math.Max(0, credit));
    }
}
