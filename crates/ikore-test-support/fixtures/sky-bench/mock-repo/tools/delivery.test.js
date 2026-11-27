import { describe, it } from "node:test";
import assert from "node:assert/strict";
import tally from "./delivery.js";

describe("Delivery", () => {
    it("skips outside entries", () => {
        assert.equal(tally([0, 86, 87]), 86);
    });
});
