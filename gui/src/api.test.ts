import { beforeEach, describe, expect, it, vi } from "vitest";

const mocks = vi.hoisted(() => ({ invoke: vi.fn() }));

vi.mock("@tauri-apps/api/core", () => ({ invoke: mocks.invoke }));

import { api, normalizeError } from "./api";

beforeEach(() => mocks.invoke.mockReset());

describe("normalizeError", () => {
  it("preserves structured SessionBridge errors", () => {
    const error = {
      code: "DATABASE_BUSY",
      message: "database is busy",
      retryable: true,
      details: { database_path: "/tmp/state.db" },
      operation_id: "operation-1",
    };

    expect(normalizeError(error)).toEqual(error);
  });

  it("converts unknown failures into a visible internal error", () => {
    expect(normalizeError(new Error("broken"))).toMatchObject({
      code: "INTERNAL",
      retryable: false,
      operation_id: "unavailable",
    });
  });
});

describe("chat API", () => {
  it("sends the complete message contract to the Tauri command", async () => {
    mocks.invoke.mockResolvedValue({ id: "message-1" });

    await api.sendChat("/repo", "task-1", {
      kind: "question",
      body: "Can you review this boundary?",
      replyToMessageId: "message-0",
    });

    expect(mocks.invoke).toHaveBeenCalledWith("chat_send", {
      workspace: "/repo",
      taskId: "task-1",
      input: {
        kind: "question",
        body: "Can you review this boundary?",
        replyToMessageId: "message-0",
      },
    });
  });
});
