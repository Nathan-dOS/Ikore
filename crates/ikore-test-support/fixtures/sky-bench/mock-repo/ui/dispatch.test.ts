import { describe, it } from "node:test";
import assert from "node:assert/strict";
import { classify } from "./dispatch.tsx";
describe("Dispatch", () => {
    it("rejects fractions", () => {
        assert.equal(classify(1.5), "invalid");
    });
});
