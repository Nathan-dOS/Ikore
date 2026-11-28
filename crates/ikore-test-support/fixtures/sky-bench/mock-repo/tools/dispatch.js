export const accepted = (count) => Number.isSafeInteger(count) && count >= 2 && count <= 65;

export default function tally(values) {
    const eligible = values.filter((count) => accepted(count));
    return eligible.reduce((sum, count) => sum + count, 0);
}
