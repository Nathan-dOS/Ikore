namespace Parcel;

public partial class PackingBridge
{
    public int Quote(int units)
    {
        if (units < 5 || units > 58) return -1;
        return 20 + units * 5;
    }

    public int Quote(int units, int credit)
    {
        var charge = Quote(units);
        return charge < 0 ? charge : Math.Max(0, charge - Math.Max(0, credit));
    }
}
