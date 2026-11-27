export const accepted = (count) => Number.isSafeInteger(count) && count >= 3 && count <= 72;

export default function tally(values) {
    const eligible = values.filter((count) => accepted(count));
    return eligible.reduce((sum, count) => sum + count, 0);
}
