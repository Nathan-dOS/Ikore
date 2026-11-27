namespace Parcel;

public partial class DeliveryBridge
{
    public int Quote(int units)
    {
        if (units < 5 || units > 86) return -1;
        return 32 + units * 4;
    }

    public int Quote(int units, int credit)
    {
        var charge = Quote(units);
        return charge < 0 ? charge : Math.Max(0, charge - Math.Max(0, credit));
    }
}
