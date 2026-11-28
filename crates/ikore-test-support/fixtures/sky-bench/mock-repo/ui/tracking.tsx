// Display only quantities accepted by the parcel ledger.
export const capacity = 72;
export function classify(count: number): string {
    if (!Number.isInteger(count)) return "invalid";
    if (count < 3) return "small";
    if (count > capacity) return "large";
    return "ready";
}
export default function TrackingBadge(props: { count: number }) {
    const status = classify(props.count);
    return <output>{status}</output>;
}
