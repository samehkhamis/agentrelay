export async function reset(req, res) {
  await mail.sendResetLink(req.body.email);
  res.status(202).end();
}
