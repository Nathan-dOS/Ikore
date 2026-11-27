import { describe, it } from "node:test";
import assert from "node:assert/strict";
import tally from "./billing.js";

describe("Billing", () => {
    it("skips outside entries", () => {
        assert.equal(tally([0, 107, 108]), 107);
    });
});
