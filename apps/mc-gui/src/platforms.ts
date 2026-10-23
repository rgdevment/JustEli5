// The meeting platforms `mc capture --source` knows, with what the user sees.

export interface PlatformInfo {
  id: string;
  label: string;
  /** How to turn captions on, in one line. */
  captions: string;
}

export const PLATFORMS: PlatformInfo[] = [
  {
    id: "teams",
    label: "Teams escritorio",
    captions: "En la reunión: Más (…) › Idioma y voz › Activar subtítulos en directo.",
  },
  {
    id: "teams-web",
    label: "Teams web",
    captions:
      "En la reunión: Más (…) › Idioma y voz › Activar subtítulos en directo; deja la pestaña al frente.",
  },
  {
    id: "meet",
    label: "Google Meet",
    captions: "Pulsa CC (Activar subtítulos) en la barra de Meet y deja la pestaña al frente.",
  },
  {
    id: "zoom",
    label: "Zoom",
    captions: "En la barra de la reunión: Subtítulos (CC) › Mostrar subtítulos.",
  },
  {
    id: "slack",
    label: "Slack",
    captions: "En la huddle: Más › Activar subtítulos.",
  },
];

export function platformLabel(id: string): string {
  return PLATFORMS.find((p) => p.id === id)?.label ?? id;
}
