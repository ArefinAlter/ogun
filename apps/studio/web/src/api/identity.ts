// Who is signed in, from `GET /api/v1/me`. No imports, so a plain `node --test` can load it.

/** `GET /api/v1/me` of ogun-server (O-13 section 6.2): the user record, the memberships, or the token's ceiling. */
export interface MeUser {
  user_id?: string
  display_name?: string
  email?: string
}
export interface MeWorkspace {
  workspace_id?: string
  name?: string
  role?: string
  approver?: boolean
}
export interface Me {
  kind?: 'session' | 'token'
  user?: MeUser
  workspaces?: MeWorkspace[]
  token?: { role?: string }
  remote_approvals?: boolean
  csrf_token?: string
  // The flat shape the frontend was first written against; still read when a server sends it.
  user_id?: string
  id?: string
  name?: string
  email?: string
  roles?: string[]
  role?: string
  approver?: boolean
}

/** Who is signed in, whichever of the two shapes arrived. A caller with several workspaces needs the
 * `x-ogun-workspace` header, which this client does not send yet, so the first membership stands for them. */
export function identityOf(me: Me): { id: string; name: string; roles: string[]; approver: boolean } {
  const ws = me.workspaces?.[0]
  const roles = me.token?.role ? [me.token.role] : ws?.role ? [ws.role] : (me.roles ?? (me.role ? [me.role] : []))
  const id = me.user?.user_id ?? me.user_id ?? me.id ?? ''
  const name = me.user?.display_name ?? me.user?.email ?? me.name ?? me.email ?? (id || 'signed in')
  // A token acts as an agent: it never decides, whatever its creator is.
  const approver = me.kind === 'token' ? false : Boolean(ws?.approver ?? me.approver)
  return { id, name, roles, approver }
}
