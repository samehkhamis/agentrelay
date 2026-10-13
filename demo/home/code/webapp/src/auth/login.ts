import type { Request, Response } from "express";
import { users } from "../db";

export async function login(req: Request, res: Response) {
  const user = await users.verify(req.body.email, req.body.password);
  if (!user) return res.status(401).json({ error: "Wrong email or password" });
  res.json({ session: await users.startSession(user) });
}
