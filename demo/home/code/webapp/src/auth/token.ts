export async function token(req, res) {
  const user = await users.verify(req.body.email, req.body.password);
  res.json({ token: user && (await tokens.issue(user)) });
}
