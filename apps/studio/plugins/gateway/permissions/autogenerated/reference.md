## Default Permission

The field gateway commands Ogun Studio uses on Android: read the status, save the configuration, start and stop
the foreground service, and request a sync. On other platforms the commands only report that the gateway is not
available.

#### This default permission set includes the following:

- `allow-status`
- `allow-configure`
- `allow-start`
- `allow-stop`
- `allow-sync-now`

## Permission Table

<table>
<tr>
<th>Identifier</th>
<th>Description</th>
</tr>


<tr>
<td>

`ogun-gateway:allow-configure`

</td>
<td>

Enables the configure command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`ogun-gateway:deny-configure`

</td>
<td>

Denies the configure command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`ogun-gateway:allow-start`

</td>
<td>

Enables the start command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`ogun-gateway:deny-start`

</td>
<td>

Denies the start command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`ogun-gateway:allow-status`

</td>
<td>

Enables the status command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`ogun-gateway:deny-status`

</td>
<td>

Denies the status command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`ogun-gateway:allow-stop`

</td>
<td>

Enables the stop command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`ogun-gateway:deny-stop`

</td>
<td>

Denies the stop command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`ogun-gateway:allow-sync-now`

</td>
<td>

Enables the sync_now command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`ogun-gateway:deny-sync-now`

</td>
<td>

Denies the sync_now command without any pre-configured scope.

</td>
</tr>
</table>
