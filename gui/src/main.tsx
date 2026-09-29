import React from "react";
import ReactDOM from "react-dom/client";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import App from "./WorkgroupApp";
import "./workgroup.css";

const queryClient = new QueryClient({
  defaultOptions: {
    queries: { networkMode: "always", retry: 1, refetchOnWindowFocus: true },
    mutations: { networkMode: "always" },
  },
});

ReactDOM.createRoot(document.getElementById("root")!).render(
  <React.StrictMode>
    <QueryClientProvider client={queryClient}>
      <App />
    </QueryClientProvider>
  </React.StrictMode>,
);
