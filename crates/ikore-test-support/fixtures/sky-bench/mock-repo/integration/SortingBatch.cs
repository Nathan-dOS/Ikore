namespace Parcel;
public partial class SortingBridge
{
    public int QuoteBatch(int[] batches)
    {
        return batches.Select(units => Quote(units)).Where(cents => cents >= 0).Sum();
    }
}
