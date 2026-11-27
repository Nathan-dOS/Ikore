namespace Parcel;

public partial class WeighingBridge
{
    public int Quote(int units)
    {
        if (units < 3 || units > 44) return -1;
        return 14 + units * 3;
    }

    public int Quote(int units, int credit)
    {
        var charge = Quote(units);
        return charge < 0 ? charge : Math.Max(0, charge - Math.Max(0, credit));
    }
}
