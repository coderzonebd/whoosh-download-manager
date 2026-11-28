# WDM usage numbers

Updated 2026-11-28 by `.github/workflows/usage-stats.yml`. Nothing here identifies a person: downloads are GitHub's counters, and active copies are counted from update checks without IP addresses or IDs (docs/privacy.md).

## Downloads (all releases)

**132** files downloaded from the releases page. Windows and Linux in-app updates download the installer too, so these include updates.

| Platform | Downloads |
|---|---:|
| windows | 43 |
| linux | 39 |
| android | 31 |
| macos | 10 |
| chromium | 9 |

## Desktop copies in use

The desktop app checks for updates once a day, so update checks per day are close to the number of copies running that day. Copies that turned the daily check off, or that a package manager updates, are not counted.

| Day (UTC) | Update checks |
|---|---:|
| 2026-11-27 | 2 |
| 2026-11-26 | 4 |
| 2026-11-25 | 2 |
| 2026-11-24 | 2 |
| 2026-11-23 | 2 |
| 2026-11-22 | 2 |
| 2026-11-21 | 8 |

Raw data: [`downloads.csv`](downloads.csv), [`active.csv`](active.csv).
