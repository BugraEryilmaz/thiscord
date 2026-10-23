; Tauri runs this after replacing the executable and updating shortcuts, including
; passive /UPDATE installs. Existing shortcuts can otherwise retain an old icon
; because the executable's path stays the same across releases.
!macro NSIS_HOOK_POSTINSTALL
  ; SHCNE_ASSOCCHANGED (0x08000000) invalidates the Shell's icon/thumbnail cache.
  ; SHCNF_IDLIST | SHCNF_FLUSH (0x1000), with both item pointers null.
  ; Use the Shell API so desktop, Start menu and pinned shortcuts can reload their
  ; icons without rewriting links, recreating deleted shortcuts or restarting Explorer.
  System::Call 'shell32::SHChangeNotify(i 0x08000000, i 0x1000, p 0, p 0)'
!macroend
