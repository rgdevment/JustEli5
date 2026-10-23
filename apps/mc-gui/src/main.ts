import { App } from "./app";
import { loadSettings } from "./settings";
import { offerUpdate } from "./update";

loadSettings().then((settings) => new App(settings).init());
offerUpdate();
