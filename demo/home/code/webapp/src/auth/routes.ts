import { Router } from "express";
import { rateLimit } from "../middleware/rateLimit";
import { login } from "./login";
import { reset } from "./reset";
import { token } from "./token";

export const router = Router();

router.post("/login", rateLimit, login);
router.post("/reset", reset);
router.post("/token", token);
