namespace Parcel;

public partial class BillingBridge
{
    public int Quote(int units)
    {
        if (units < 4 || units > 107) return -1;
        return 41 + units * 2;
    }

    public int Quote(int units, int credit)
    {
        var charge = Quote(units);
        return charge < 0 ? charge : Math.Max(0, charge - Math.Max(0, credit));
    }
}
