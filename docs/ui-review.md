# Browser UI and UX review

Reviewed the embedded production SPA in local Chrome with 96 synthetic requests, eight conversations, four users, three models, tool calls and rate-limit failures. No user data or paid provider requests were used.

The desktop layout has a consistent visual language: a stable sidebar, restrained colors, readable cards, and distinct investigation and administration areas. Session transcripts, usage coverage and request details support the main auditing workflow. The original mobile layout and keyboard behavior needed corrections.

## Findings and changes

| Finding observed in Chrome | Change |
| --- | --- |
| Long identifiers widened session and request pages to 568–585 CSS pixels on a 390-pixel viewport. | Wrap long values within cards; keep session identity compact and retain full identifiers in expandable details. |
| Query sort controls collapsed the selected field to an unreadable sliver. | Give the direction selector only its required width and label the query controls. |
| Populated Query results expanded the desktop page to 2,309 pixels. | Constrain the results grid column and keep horizontal scrolling inside its panel. |
| Mobile request filters occupied nearly the entire first viewport. | Use a two-column filter layout with full-width search and larger touch controls. |
| Session links emphasized opaque hashes. | Lead with user names and a short session ID; identify the user in the detail header and add a transcript shortcut. |
| Keyboard arrows did not switch tabs; Escape did not close the mobile menu. | Add tab keyboard navigation, focus containment and restoration, inert hidden navigation, an explicit close button and a skip link. |
| Request-filter and session-search labels were not associated with their controls. | Associate shared field labels and give session search an accessible name. |
| Returning from a detail page lost list filters and pagination. | Preserve the list URL in detail links and validate the return destination. |
| Export appeared to export all results and reset the page offset. | Export the current page and label its scope explicitly. |
| The login screen offered SSO when it was disabled. | Add a public capability endpoint exposing only local/SSO availability; show configured sign-in methods. |
| Overview text implied all counters covered 24 hours. | Distinguish retained history, 24-hour investigations and counters since process startup. |
| Dark purple links measured only 3.98:1 against their card background. | Increase dark-theme link contrast to 5.95:1 and darken light-theme status text. |
| Charts retained dark-theme colors after switching to light. | Make chart colors depend on the selected theme. |
| Raw protocol enum names and large millisecond values were hard to scan. | Use readable request-kind names and duration formatting on session details. |

## Validation

Chrome 151.0.7922.71 passed all 28 browser assertions using the embedded release build. The run covers 1,440 × 1,000 desktop, 390 × 844 mobile and 320 × 740 narrow viewports, with 34 updated screenshots. No console/runtime errors or page-level horizontal overflow were recorded. See the [machine-readable report](ui-review/after/report.json). Timing entries record deliberate screenshot settling waits.

Checks include sign-in/logout, configured login methods, user search, request filters, pagination, returning to filtered lists, JSONL IDs matching the current page, lazy/shared payload loading, tool inspection, user cost analytics, structured queries, empty results, theme switching, keyboard tabs, mobile-menu focus behavior and the transcript shortcut.

Also passed: 43 frontend tests, Svelte diagnostics (zero errors/warnings), the production frontend build, 351 Rust unit tests, the PostgreSQL-backed auth/proxy integration test, Clippy with warnings denied, the Rust release build, and `git diff --check`. The browser fixture has been stopped and its isolated database cleaned up.

## Screenshots

Compare [original mobile session](ui-review/before/mobile-session.png) with [updated mobile session](ui-review/after/mobile-session.png), and [original mobile filters](ui-review/before/mobile-requests.png) with [updated mobile filters](ui-review/after/mobile-requests.png).

Other views: [desktop overview](ui-review/after/desktop-overview.png), [session investigation](ui-review/after/desktop-session.png), [user analytics](ui-review/after/desktop-employee-analytics.png), [mobile transcript](ui-review/after/mobile-transcript.png), [light theme](ui-review/after/desktop-overview-light.png).

## Reproducing the review

Requires local PostgreSQL, Node with built-in WebSocket support, and Chrome (`CHROME_BIN` can override `/usr/bin/google-chrome`). The SQLx fixture creates and drops a separate database. Use an empty directory for each run.

```sh
corepack pnpm --dir crates/llmtrace/ui run build
cargo build --release -p llmtrace
# In terminal 1; DATABASE_URL must point to a local test PostgreSQL instance:
LLMTRACE_UI_REVIEW_DIR=/tmp/llmtrace-ui-review cargo test --workspace browser_ui_review -- --ignored --nocapture
# In terminal 2, after fixture.json appears:
LLMTRACE_UI_VERIFY=1 node scripts/browser-ui-review.mjs /tmp/llmtrace-ui-review/fixture.json docs/ui-review/after
# Stop the fixture and allow SQLx to clean up:
touch /tmp/llmtrace-ui-review/stop
```

The fixture exits immediately without `LLMTRACE_UI_REVIEW_DIR`. An enabled fixture also has a one-hour deadline. Browser downloads and profiles stay in temporary directories and are deleted after the run. Screenshots and reports contain synthetic content only.

## Remaining product work

The overview still emphasizes traffic and operational health. An enterprise landing page would benefit from user spend, attribution coverage and links to sessions needing review. Dense analytics tables intentionally scroll horizontally on small screens. Request summaries could expose user identity and cost directly, and saved investigations would reduce repeated filtering.

This is a Chrome review with responsive viewport emulation, not a Safari/Firefox, physical-device or screen-reader certification. Real OIDC sign-in requires a configured identity provider and was not exercised here.
