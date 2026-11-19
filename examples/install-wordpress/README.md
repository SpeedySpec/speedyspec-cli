# install-wordpress

An example speedyspec plugin: it makes `speedyspec` able to install WordPress.

It registers one app, `wordpress`, and answers the zip driver with the official archive
(`https://wordpress.org/latest.zip`), unpacked into the base directory with its wrapping
`wordpress/` directory removed. The CLI does the downloading and unpacking; the plugin only
says what and where.

Run it straight from a checkout:

```bash
speedyspec --plugin-dir examples/install-wordpress app install wordpress --base /srv/site
```

The base directory must be empty, or pass `--force` to merge into it (files of the same path
are replaced, nothing is deleted). On success the command prints the destination and the
absolute path it was installed to.

This example only downloads WordPress. The full plugin, which also writes `wp-config.php`, reaches the
database and manages plugin and theme files, is
[speedyspec/wordpress](https://github.com/SpeedySpec/speedyspec-cli-wordpress).
