# WDM usage numbers

Updated 2026-11-22 by `.github/workflows/usage-stats.yml`. Nothing here identifies a person: downloads are GitHub's counters, and active copies are counted from update checks without IP addresses or IDs (docs/privacy.md).

## Downloads (all releases)

**80** files downloaded from the releases page. Windows and Linux in-app updates download the installer too, so these include updates.

| Platform | Downloads |
|---|---:|
| linux | 29 |
| windows | 22 |
| android | 16 |
| macos | 8 |
| chromium | 5 |

## Desktop copies in use

The desktop app checks for updates once a day, so update checks per day are close to the number of copies running that day. Copies that turned the daily check off, or that a package manager updates, are not counted.

| Day (UTC) | Update checks |
|---|---:|
| 2026-11-21 | 8 |

Raw data: [`downloads.csv`](downloads.csv), [`active.csv`](active.csv).
