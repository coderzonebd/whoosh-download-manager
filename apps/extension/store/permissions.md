# Permission justifications

For the Chrome Web Store "Privacy practices" tab and AMO review notes. The same text applies to both stores; Firefox shows host access as "Access your data for all websites".

**Single purpose (Chrome Web Store):** Whoosh hands the browser's downloads to the Whoosh download manager app installed on the same computer, with the cookies and referrer the browser would have used, so large and interrupted downloads finish faster and reliably.

| Permission | Justification |
|---|---|
| `downloads` | To see that the browser started a download, and to cancel and remove the browser's copy once the Whoosh app has accepted it (never before, so no download is lost). Also to let the browser download a link Whoosh cannot take. |
| `nativeMessaging` | The only channel to the Whoosh app on the same computer (`com.coderzonebd.whoosh`). The extension has no other network communication of its own. |
| `cookies` | To read the cookies for the address being downloaded, so the Whoosh app can repeat the request as the browser would (sites that need a login or a session cookie). Read only for that address, never stored. |
| `webRequest` | Read-only, non-blocking. (1) To notice downloads that came from a form submission (POST), which stay in the browser. (2) The optional media detector: response headers of audio, video and streaming-playlist requests, to list them in the popup. Nothing is modified or blocked. |
| `contextMenus` | The menu items "Download link with Whoosh", "Download selected links with Whoosh" and "Download all links with Whoosh". |
| `scripting` | To add the link-collecting content script to tabs that were open before the extension was installed, when the user uses a context-menu item there. |
| `storage` | The user's settings; a list of downloads handed over (for repairing expired links, see the privacy policy); detected media per tab in session storage. |
| Host access `http://*/*`, `https://*/*` | Downloads can come from any site. Needed for the cookies of the downloaded address, for the content script (Alt+click, hover-to-prepare, link collection for the context menus), and to read the page a download came from when its address expired (self-healing). |

**Remote code:** none. All code is in the package; the bundles are not minified so they can be reviewed.

**Data use (Chrome Web Store disclosures):** the extension does not collect or transmit user data to the developer or third parties. It passes download addresses, cookies for those addresses, the referrer and the user agent to a program on the user's own computer (the Whoosh app) — that is the extension's single purpose. Answers for the form: no personally identifiable information, health, financial, authentication, personal communications, location or web history is collected; "website content" and "user activity" are handled only locally, for the single purpose. Not sold, not used for anything else, not used for credit decisions.

**Firefox `data_collection_permissions`:** `required: ["none"]` — nothing is transmitted to the developer or third parties.

**Store build:** the Web Store / AMO packages (`build:store`) do not capture, detect or offer downloads on YouTube or similar streaming sites (list in `src/shared/store-blocklist.json`, MASTER-PLAN §6), and do not run their content script there.
