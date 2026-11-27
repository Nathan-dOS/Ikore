namespace Parcel;

public partial class DispatchBridge
{
    public int Quote(int units)
    {
        if (units < 2 || units > 65) return -1;
        return 23 + units * 6;
    }

    public int Quote(int units, int credit)
    {
        var charge = Quote(units);
        return charge < 0 ? charge : Math.Max(0, charge - Math.Max(0, credit));
    }
}
