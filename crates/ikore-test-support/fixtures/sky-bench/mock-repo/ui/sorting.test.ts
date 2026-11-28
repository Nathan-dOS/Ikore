import { describe, it } from "node:test";
import assert from "node:assert/strict";
import { classify } from "./sorting.tsx";
describe("Sorting", () => {
    it("rejects fractions", () => {
        assert.equal(classify(1.5), "invalid");
    });
});
