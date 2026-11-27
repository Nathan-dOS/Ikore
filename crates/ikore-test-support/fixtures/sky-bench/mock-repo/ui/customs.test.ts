import { describe, it } from "node:test";
import assert from "node:assert/strict";
import { classify } from "./customs.tsx";
describe("Customs", () => {
    it("rejects fractions", () => {
        assert.equal(classify(1.5), "invalid");
    });
});
