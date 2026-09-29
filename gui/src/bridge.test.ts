import { beforeEach, describe, expect, it, vi } from "vitest";
import {
    call,
    errorText,
    mergeHistory,
    type History,
    type Message,
} from "./bridge";
import { invoke } from "@tauri-apps/api/core";
vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
describe("workspace transport", () => {
    beforeEach(() => vi.clearAllMocks());
    it("preserves the request ID on explicit retries without exposing credentials", async () => {
        vi.mocked(invoke).mockResolvedValue({ id: "message" });
        const args = {
            request_id: "same-request",
            body: "hello",
            recipients: ["worker"],
        };
        await call("/project", "message.send", args);
        await call("/project", "message.send", args);
        expect(invoke).toHaveBeenNthCalledWith(1, "bridge_call", {
            workspace: "/project",
            operation: "message.send",
            args,
        });
        expect(invoke).toHaveBeenNthCalledWith(2, "bridge_call", {
            workspace: "/project",
            operation: "message.send",
            args,
        });
    });
    it("propagates backend failures", async () => {
        const error = { code: "HANDSHAKE", message: "challenge rejected" };
        vi.mocked(invoke).mockRejectedValue(error);
        await expect(call("/project", "member.initialize")).rejects.toEqual(
            error,
        );
        expect(errorText(error)).toBe("challenge rejected");
    });
});

function page(ids: number[], cursor: number, hasMore = false): History {
    return {
        messages: ids.map(
            (seq) =>
                ({ id: String(seq), seq, body: `message ${seq}` }) as Message,
        ),
        deliveries: [],
        cursor: ids.at(-1) ?? 0,
        change_cursor: cursor,
        has_more: hasMore,
    };
}
describe("history pagination", () => {
    it("does not skip a gap when an unloaded old message changes", () => {
        const initial = mergeHistory(undefined, page([100, 101], 5, true));
        const updated = mergeHistory(initial, page([1, 101, 102], 6));
        expect(updated.messages.map((m) => m.seq)).toEqual([100, 101, 102]);
        expect(updated.older_available).toBe(true);
    });
    it("deduplicates old pages and preserves newer contents and the live cursor", () => {
        const live = mergeHistory(undefined, page([100, 101], 5, true));
        live.messages[0].body = "new contents";
        const older = mergeHistory(live, page([99, 100], 20), true);
        expect(older.messages.map((m) => m.seq)).toEqual([99, 100, 101]);
        expect(older.messages[1].body).toBe("new contents");
        expect(older.change_cursor).toBe(5);
        const refreshed = mergeHistory(older, page([101, 102], 6));
        expect(refreshed.messages.map((m) => m.seq)).toEqual([
            99, 100, 101, 102,
        ]);
    });
});
