import { describe, expect, it } from "vitest";
import { buildDocsUrl, DEFAULT_DOCS_SOURCE_BASE } from "./docs-url";

describe("buildDocsUrl", () => {
  it("links to the pinned MDX source by default", () => {
    expect(buildDocsUrl("gateway/guides/episodes")).toBe(
      `${DEFAULT_DOCS_SOURCE_BASE}/gateway/guides/episodes.mdx`,
    );
    expect(DEFAULT_DOCS_SOURCE_BASE).not.toContain("tensorzero.com");
  });

  it("keeps anchors after the file extension", () => {
    expect(buildDocsUrl("gateway/create-a-prompt-template#migrate")).toBe(
      `${DEFAULT_DOCS_SOURCE_BASE}/gateway/create-a-prompt-template.mdx#migrate`,
    );
  });

  it("uses an operator docs base without an extension", () => {
    expect(
      buildDocsUrl("/gateway/guides/episodes/", "https://docs.example.com/"),
    ).toBe("https://docs.example.com/gateway/guides/episodes");
  });

  it("ignores a blank operator base", () => {
    expect(buildDocsUrl("a/b", "  ")).toBe(`${DEFAULT_DOCS_SOURCE_BASE}/a/b.mdx`);
  });
});
