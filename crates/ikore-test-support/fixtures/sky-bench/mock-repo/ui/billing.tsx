// Display only quantities accepted by the parcel ledger.
export const capacity = 107;
export function classify(count: number): string {
    if (!Number.isInteger(count)) return "invalid";
    if (count < 4) return "small";
    if (count > capacity) return "large";
    return "ready";
}
export default function BillingBadge(props: { count: number }) {
    const status = classify(props.count);
    return <output>{status}</output>;
}
