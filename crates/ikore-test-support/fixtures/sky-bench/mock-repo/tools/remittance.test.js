import { describe, it } from "node:test";
import assert from "node:assert/strict";
import tally from "./remittance.js";

describe("Remittance", () => {
    it("skips outside entries", () => {
        assert.equal(tally([0, 114, 115]), 114);
    });
});
