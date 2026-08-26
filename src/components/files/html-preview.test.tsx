import { afterEach, describe, expect, it, vi } from "vitest"
import {
  cleanup,
  fireEvent,
  render,
  screen,
  waitFor,
} from "@testing-library/react"
import type { FileWorkspaceTab } from "@/contexts/workspace-context"

vi.mock("next-intl", () => ({ useTranslations: () => (k: string) => k }))

// The fixture document references no sub-resources, so the reader is never
// asked — if it is, someone changed the inliner contract.
vi.mock("@/lib/api", () => ({
  readWorkspaceFileBase64: vi.fn(async () => {
    throw new Error("should not be reached")
  }),
}))

import { HtmlPreview } from "./html-preview"

// The untrusted mode must render markup without executing anything; the
// trusted mode is a narrow, opaque-origin allow-list — never same-origin,
// never top-navigation (ADR 0001 blocker #4).
const BLOCKED_SANDBOX = ""
const TRUSTED_SANDBOX = "allow-scripts allow-popups allow-forms allow-modals"

afterEach(cleanup)

function tabWith(content: string): FileWorkspaceTab {
  return {
    id: "html-1",
    kind: "file",
    folderId: null,
    title: "preview.html",
    description: null,
    path: "/proj/preview.html",
    language: "html",
    content,
    loading: false,
  }
}

async function renderPreview(content: string): Promise<HTMLIFrameElement> {
  render(<HtmlPreview tab={tabWith(content)} rootPath={null} />)
  return waitFor(() => {
    const iframe = document.querySelector("iframe")
    if (!iframe) throw new Error("iframe not rendered")
    return iframe as HTMLIFrameElement
  })
}

describe("HtmlPreview sandbox default (ADR 0001 blocker #4)", () => {
  it("blocks all script execution unless the file is explicitly trusted", async () => {
    const iframe = await renderPreview(
      "<html><body><p>hello</p><script>window.pwned = true</script></body></html>"
    )

    expect(iframe.getAttribute("sandbox")).toBe(BLOCKED_SANDBOX)
    const srcDoc = iframe.getAttribute("srcdoc") ?? ""
    expect(srcDoc).toContain("script-src 'none'")
    expect(srcDoc).not.toContain("allow-scripts")
    expect(screen.getByRole("button").getAttribute("aria-pressed")).toBe(
      "false"
    )
  })

  it("grants only the restricted sandbox after explicit trust, and revokes it again", async () => {
    await renderPreview("<html><body>hello</body></html>")

    fireEvent.click(screen.getByRole("button"))
    await waitFor(() =>
      expect(document.querySelector("iframe")?.getAttribute("sandbox")).toBe(
        TRUSTED_SANDBOX
      )
    )
    const trusted =
      document.querySelector("iframe")?.getAttribute("srcdoc") ?? ""
    expect(trusted).toContain("script-src 'unsafe-inline'")
    expect(trusted).not.toContain("allow-same-origin")
    expect(trusted).not.toContain("allow-top-navigation")

    fireEvent.click(screen.getByRole("button"))
    await waitFor(() =>
      expect(document.querySelector("iframe")?.getAttribute("sandbox")).toBe(
        BLOCKED_SANDBOX
      )
    )
  })
})
