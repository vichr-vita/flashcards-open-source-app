# Review card design QA

The review card is larger, has a theme-specific shadow, and turns over on reveal. The comparison
passes with no remaining P0, P1, or P2 findings in the tested review component flow.

## Evidence and scope

- Source visual: `/home/vichr/.codex/generated_images/01a0fc2a-b3d8-7201-ba3e-a849130551a3/exec-55201ac3-ae02-4033-b631-db6b9609d5f8.png`.
- Browser implementation, front: [dark](tmp/review-ui-qa/dark-front.png) and [light](tmp/review-ui-qa/light-front.png).
- Browser implementation, back: [desktop](tmp/review-ui-qa/dark-back.png) and [mobile](tmp/review-ui-qa/dark-back-mobile.png).
- Full comparison: [source on the left, implementation on the right](tmp/review-ui-qa/comparison-desktop.png).
- Focused comparison: [card and reveal action](tmp/review-ui-qa/comparison-card.png).
- Additional state: [light mobile front](tmp/review-ui-qa/light-front-mobile.png).

The source is 1514 × 1039 pixels. The desktop browser viewport and screenshots are 1514 × 1033
CSS pixels and pixels, at device scale factor 1. The combined comparison removes only the bottom
six blank pixels from the source. Neither image is rescaled. Both show the same question, tags,
dark theme, and unrevealed state. The focused comparison uses the same rectangle from each image.

The isolated fixture runs the production review pane, header, filters, queue panel, keyboard
handlers, markdown renderer, localization, and styles with an in-memory queue. The fixture's
header navigation supplies shell context; it does not verify authenticated navigation, sync,
editing persistence, AI requests, or synthesized audio playback. The existing application keeps
those behaviors. The fixture uses the reference's purple accent; production retains the user's
existing accent preference. No raster artwork is required by the selected screen.

## Comparison history

1. The first capture had a one-line question and undersized utility text. The mobile rating
   controls used four rows and covered the card's lower controls. These were P2 findings.
   The fixes constrain compact prompts to 20 characters of typographic width, enlarge the
   relevant text and card proportions, and keep mobile ratings in two columns. The final
   focused comparison and mobile back capture show the corrected layout.
2. The second comparison showed a sharp background strip cutting across the light card's
   shadow above the reveal button. This was a P2 elevation mismatch. The action dock now has
   a transparent background; the rating grid supplies its own background for long content.
   The final light front capture shows a continuous shadow and a separate reveal action.
3. The final full and focused comparisons show the same primary hierarchy, two-line prompt,
   wide card, plain metadata, and reveal action directly beneath it. The card and actions fit
   on desktop, laptop, and mobile. No further P0, P1, or P2 visual fixes are required.

## Fidelity surfaces

| Surface | Result |
| --- | --- |
| Fonts and typography | The existing system font stack remains. The compact prompt has large, left-aligned type and wraps like the reference. Native font metrics and utility text weights vary by OS. |
| Spacing and layout | The centered card reaches about 1090 pixels wide at the comparison viewport. Its rounded border, generous inset, and nearby action reproduce the reference's proportions. Mobile controls remain reachable. |
| Colors and tokens | Dark mode uses a true black page and charcoal card with a subtle accent-colored shadow. Light mode uses a white card, dark text, a fine border, and a stronger neutral shadow. Existing theme and accent preferences remain authoritative. |
| Assets and icons | No new raster assets are needed. Edit and audio controls use the installed icon library. Existing queue, progress, and repetition icons retain their product meaning. The card and button use flat fills. |
| Copy and content | The question and metadata match the reference. The implementation uses existing localized labels and removes the review subtitle. No new translation keys are needed. |

## Interaction verification

`npm run test:e2e:review-ui` passed all 10 Playwright checks in Chromium. Desktop is 1514 × 1033,
laptop is 1280 × 800, and mobile is 390 × 844 with touch enabled. Both dark and light themes pass.

The checks cover button and Space reveal, an intermediate Y-axis rotation frame, the final back
face, inert and hidden front controls, reduced-motion reveal without a transition, rating by
keyboard, advancement to a fresh front face, horizontal overflow, and scrolling to the final
section of a long markdown answer while keeping ratings reachable. No page or console errors
were recorded in the primary flow.

The web build, existing review checks, all five repository static checks, workflow lint, and diff
whitespace check pass. PR checks now run the browser flow and retain its screenshots and failure
traces. The new CI configuration has not been run on GitHub in this task.

## Follow-up polish and limits

P3 differences are the OS-specific font metrics and the reference's soft surface shading versus
the implementation's flat fills. The existing floating chat entry is outside this isolated
fixture. Safari, Firefox, and a live authenticated review were not exercised in this task.

## Implementation checklist

- [x] Match the selected card composition and simplify review chrome.
- [x] Keep the card prominent in both themes.
- [x] Rotate around the Y axis on reveal and respect reduced motion.
- [x] Start a replacement card on its front and stop speech from the hidden face.
- [x] Verify responsive layouts and long answers in a real browser.
- [x] Add the focused flow to CI and retain screenshots.

final result: passed
