interface StatusOverlaysProps {
  isStopping: boolean;
  isSaving: boolean;
  sidebarCollapsed: boolean;
}

export function StatusOverlays({ isStopping, isSaving, sidebarCollapsed }: StatusOverlaysProps) {
  const message = isStopping ? 'Stopping recording…' : isSaving ? 'Saving meeting…' : null;
  if (!message) return null;

  return (
    <div className="fixed bottom-4 left-0 right-0 z-10">
      <div
        className="flex justify-center pl-8 transition-[margin] duration-300"
        style={{ marginLeft: sidebarCollapsed ? '4rem' : '16rem' }}
      >
        <div className="w-2/3 max-w-[750px] flex justify-center">
          <div className="bg-white rounded-full shadow-lg px-4 py-2 flex items-center space-x-2">
            <div className="animate-spin rounded-full h-4 w-4 border-b-2 border-gray-900"></div>
            <span className="text-sm text-gray-700">{message}</span>
          </div>
        </div>
      </div>
    </div>
  );
}
