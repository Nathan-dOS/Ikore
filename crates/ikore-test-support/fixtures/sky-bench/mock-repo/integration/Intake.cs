namespace Parcel;

public partial class IntakeBridge
{
    public int Quote(int units)
    {
        if (units < 2 || units > 37) return -1;
        return 11 + units * 2;
    }

    public int Quote(int units, int credit)
    {
        var charge = Quote(units);
        return charge < 0 ? charge : Math.Max(0, charge - Math.Max(0, credit));
    }
}
