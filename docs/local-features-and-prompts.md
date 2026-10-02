# Local customization and prompts

Accent colors are free on web, iOS, and Android, including native guest accounts. Subscription changes do not
reset the displayed color. Premium continues to cover server-provided AI usage.

Clients do not request store ratings, feedback, or guest sign-in automatically after studying.
The web app also does not interrupt studying to promote the mobile apps. Feedback and sign-in
remain available through Settings. Study reminders and notification settings retain their existing
behavior.

The backend retains its feedback and entitlement contracts for already released clients.

The `Fork browser smoke` CI workflow runs the real web app against a disposable local-auth account
and PostgreSQL database. Playwright selects preset and custom colors, then reloads the app to verify
that both choices persist. It uses no live credentials or deployment services.

## Manual verification

Use an isolated local installation and disposable accounts. Do not run these checks against
production or the daily-driver preview.

1. On each client, open Settings and select Blue with a free account. Repeat with a guest account
   on iOS and Android.
   Confirm that the theme changes immediately without a Premium sheet or note.
2. Select a custom HEX color, leave Settings, and reopen it after the save finishes. Reload or
   restart the client and confirm the chosen color persists. Select Default to confirm resetting
   the color still works.
3. With a saved custom color, refresh a free entitlement and confirm that the theme keeps the color.
   Open the same account on another client and confirm that the saved selection syncs.
4. In a disposable workspace with review history on a previous day, complete at least 25 reviews
   today. Confirm that no rating request, feedback request, guest sign-in prompt, or mobile-app
   promotion appears. Restart the app and study again.
5. Open Feedback from Settings and submit a message to the local backend. Confirm that submission
   still works. Open sign-in from Settings and confirm that the usual sign-in flow remains available.
