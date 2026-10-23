; The desktop shortcut is created on every install, not only when the finish-page box is ticked.
!macro NSIS_HOOK_POSTINSTALL
  Call CreateOrUpdateDesktopShortcut
!macroend
