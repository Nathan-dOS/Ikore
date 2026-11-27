namespace Parcel;
public partial class CustomsBridge
{
    public int QuoteBatch(int[] batches)
    {
        return batches.Select(units => Quote(units)).Where(cents => cents >= 0).Sum();
    }
}
