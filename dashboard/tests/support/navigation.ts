import type { Page } from "@playwright/test";

/** Follow the same primary/All tools navigation available to an operator. */
export async function navigateTo(page: Page, title: string) {
  if (
    (page.viewportSize()?.width ?? 1280) <= 680 &&
    !["Overview", "Inventory", "Scanner", "Fulfillment"].includes(title)
  ) {
    await page.getByRole("button", { name: "All tools", exact: true }).click();
    await page
      .getByRole("dialog", { name: "All tools", exact: true })
      .getByRole("button", { name: title, exact: true })
      .click();
  } else {
    await page.getByRole("button", { name: title, exact: true }).click();
  }
}
