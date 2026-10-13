import type { Request, Response, NextFunction } from "express";
import { log } from "../log";

const WINDOW_MS = 60_000;
const LIMIT = 5;
const hits = new Map<string, number[]>();

/** Allows LIMIT requests per WINDOW_MS per client IP, then answers 429. */
export function rateLimit(req: Request, res: Response, next: NextFunction) {
  const now = Date.now();
  const recent = (hits.get(req.ip) ?? []).filter((t) => now - t < WINDOW_MS);
  if (recent.length >= LIMIT) {
    log.warn("login rate limited", { ip: req.ip });
    res.set("Retry-After", "60").status(429).json({ error: "Too many login attempts" });
    return;
  }
  hits.set(req.ip, [...recent, now]);
  next();
}
