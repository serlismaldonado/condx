import { defineApp } from "convex/server";
import condx from "../../src/component/convex.config";

const app = defineApp();
app.use(condx);
export default app;
