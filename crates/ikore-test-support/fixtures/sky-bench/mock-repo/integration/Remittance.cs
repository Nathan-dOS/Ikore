namespace Parcel;

public partial class RemittanceBridge
{
    public int Quote(int units)
    {
        if (units < 5 || units > 114) return -1;
        return 44 + units * 3;
    }

    public int Quote(int units, int credit)
    {
        var charge = Quote(units);
        return charge < 0 ? charge : Math.Max(0, charge - Math.Max(0, credit));
    }
}
