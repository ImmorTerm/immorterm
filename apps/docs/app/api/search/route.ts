import { createFromSource } from "fumadocs-core/search/server";
import { source } from "@/lib/source";

/** Static, client-side search (no server at runtime — fits the CF Pages static
 *  export). `staticGET` emits the search index as static JSON at /api/search;
 *  the client (RootProvider `type: "static"`) fetches it once and runs Orama
 *  in the browser. */
export const revalidate = false;
export const { staticGET: GET } = createFromSource(source);
