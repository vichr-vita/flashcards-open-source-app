// Only the workspace path builder consumes app data in this isolated browser fixture.
// Review components, markdown, localization, filters, and keyboard handlers run unchanged.
export function useAppData() {
  return { activeWorkspace: { workspaceId: "review-ui-workspace" } };
}
