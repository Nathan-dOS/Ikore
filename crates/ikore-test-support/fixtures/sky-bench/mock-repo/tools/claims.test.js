import { describe, it } from "node:test";
import assert from "node:assert/strict";
import tally from "./claims.js";

describe("Claims", () => {
    it("skips outside entries", () => {
        assert.equal(tally([0, 100, 101]), 100);
    });
});
