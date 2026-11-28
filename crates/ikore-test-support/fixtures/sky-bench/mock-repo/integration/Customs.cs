namespace Parcel;

public partial class CustomsBridge
{
    public int Quote(int units)
    {
        if (units < 4 || units > 79) return -1;
        return 29 + units * 3;
    }

    public int Quote(int units, int credit)
    {
        var charge = Quote(units);
        return charge < 0 ? charge : Math.Max(0, charge - Math.Max(0, credit));
    }
}
