#![allow(clippy::expect_used)]

use std::{
  fs,
  io::Write as _,
  path::{Path, PathBuf},
  process::{Child, Command, ExitStatus, Output, Stdio},
  thread,
  time::{Duration, Instant, SystemTime, UNIX_EPOCH}
};

use anyhow::{Context, Result, bail};
use omnifuse_wiki::client::Client;

const ENABLE_REAL_MOUNT_SMOKE: &str = "OMNIFUSE_REAL_MOUNT_SMOKE";
const ENABLE_REAL_WIKI_MOUNT_SMOKE: &str = "OMNIFUSE_REAL_WIKI_MOUNT_SMOKE";
const SMOKE_TIMEOUT: Duration = Duration::from_secs(30);
const POLL_INTERVAL: Duration = Duration::from_millis(250);

#[test]
fn git_mount_append_is_pushed_to_bare_remote() -> Result<()> {
  if std::env::var(ENABLE_REAL_MOUNT_SMOKE).as_deref() != Ok("1") {
    eprintln!("SKIP: set {ENABLE_REAL_MOUNT_SMOKE}=1 to run the real FUSE CLI smoke test");
    return Ok(());
  }

  let of_bin = assert_cmd::cargo::cargo_bin!("of");
  if !of_check_passes(&of_bin)? {
    eprintln!("SKIP: `of check` reported that FUSE/WinFsp is unavailable");
    return Ok(());
  }

  let tmp = tempfile::Builder::new()
    .prefix("omnifuse-real-mount-smoke-")
    .tempdir_in("/tmp")
    .context("create smoke tempdir under /tmp")?
    .keep();
  let bare_remote = tmp.join("omnifuse-git-remote.git");
  let seed_clone = tmp.join("omnifuse-git-seed");
  let verify_clone = tmp.join("omnifuse-git-verify");
  let mountpoint = tmp.join("mount").join("git");
  fs::create_dir_all(&mountpoint).context("create mountpoint")?;

  seed_bare_remote(&bare_remote, &seed_clone)?;

  let mut mount = CliMount::spawn_git(&of_bin, &bare_remote, &mountpoint, &tmp)?;
  let mounted_readme = mountpoint.join("README.md");

  wait_until("mounted README.md becomes readable", SMOKE_TIMEOUT, || {
    mount.ensure_running()?;
    path_is_file_with_timeout(&mounted_readme, Duration::from_secs(2))
  })
  .with_context(|| mount.diagnostics())?;

  let marker = smoke_marker()?;
  append_marker(&mounted_readme, &marker)?;

  wait_until("appended marker is pushed to the bare remote", SMOKE_TIMEOUT, || {
    mount.ensure_running()?;
    let content = git_show(&bare_remote, "main:README.md")?;
    Ok(content.contains(&marker))
  })
  .with_context(|| mount.diagnostics())?;

  clone_remote(&bare_remote, &verify_clone)?;
  let verified = fs::read_to_string(verify_clone.join("README.md")).context("read verified README.md")?;
  assert!(
    verified.contains(&marker),
    "verified clone must contain the marker written through the mountpoint"
  );

  mount.stop()?;
  cleanup_smoke_dir(&tmp);
  Ok(())
}

#[test]
fn wiki_mount_nvim_and_concurrent_api_edits_are_synced() -> Result<()> {
  if std::env::var(ENABLE_REAL_WIKI_MOUNT_SMOKE).as_deref() != Ok("1") {
    eprintln!("SKIP: set {ENABLE_REAL_WIKI_MOUNT_SMOKE}=1 to run the real Wiki FUSE CLI smoke test");
    return Ok(());
  }

  let of_bin = assert_cmd::cargo::cargo_bin!("of");
  if !of_check_passes(&of_bin)? {
    eprintln!("SKIP: `of check` reported that FUSE/WinFsp is unavailable");
    return Ok(());
  }

  let config = RealWikiSmokeConfig::from_env()?;
  let rt = tokio::runtime::Runtime::new().context("create tokio runtime for Wiki smoke")?;
  let client = Client::new(&config.base_url, &config.token, config.org_id.as_deref()).context("create Wiki client")?;
  let test_slug = config.test_slug()?;
  let api_slug = format!("{test_slug}/api-concurrent");
  let title = "OmniFuse mount smoke";
  let initial_content = "# OmniFuse mount smoke\n\nTemporary page for CLI mount smoke.\n";
  let page = rt
    .block_on(client.create_page(&test_slug, title, Some(initial_content), "page"))
    .with_context(|| format!("create disposable Wiki page {test_slug}"))?;
  let api_page = match rt.block_on(client.create_page(
    &api_slug,
    "OmniFuse concurrent API page",
    Some("# API page\n\nInitial content.\n"),
    "page"
  )) {
    Ok(page) => page,
    Err(error) => {
      let _ = rt.block_on(client.delete_page(page.id));
      return Err(error).with_context(|| format!("create disposable Wiki API page {api_slug}"));
    }
  };

  let result = run_real_wiki_mount_smoke(&of_bin, &config, &client, &rt, &test_slug, &api_slug, api_page.id);

  if let Err(error) = rt.block_on(client.delete_page(api_page.id)) {
    eprintln!("WARN: failed to delete disposable Wiki page {api_slug}: {error}");
  }
  if let Err(error) = rt.block_on(client.delete_page(page.id)) {
    eprintln!("WARN: failed to delete disposable Wiki page {test_slug}: {error}");
  }

  result
}

#[test]
fn wiki_mount_editorial_collaboration_flow_is_synced() -> Result<()> {
  if std::env::var(ENABLE_REAL_WIKI_MOUNT_SMOKE).as_deref() != Ok("1") {
    eprintln!("SKIP: set {ENABLE_REAL_WIKI_MOUNT_SMOKE}=1 to run the real Wiki FUSE CLI smoke test");
    return Ok(());
  }

  let of_bin = assert_cmd::cargo::cargo_bin!("of");
  if !of_check_passes(&of_bin)? {
    eprintln!("SKIP: `of check` reported that FUSE/WinFsp is unavailable");
    return Ok(());
  }

  let config = RealWikiSmokeConfig::from_env()?;
  let rt = tokio::runtime::Runtime::new().context("create tokio runtime for Wiki editorial smoke")?;
  let client = Client::new(&config.base_url, &config.token, config.org_id.as_deref()).context("create Wiki client")?;
  let root_slug = config.editorial_test_slug()?;
  let mut created_pages = Vec::new();

  let result = (|| -> Result<()> {
    create_disposable_wiki_page(
      &rt,
      &client,
      &mut created_pages,
      &root_slug,
      "OmniFuse editorial smoke",
      "# Редакционный проект\n"
    )?;
    let plan_slug = format!("{root_slug}/plan");
    let article_slug = format!("{root_slug}/article");
    let media_slug = format!("{root_slug}/media");
    let telegram_slug = format!("{media_slug}/telegram");
    let vk_slug = format!("{media_slug}/vk");

    let plan_id = create_disposable_wiki_page(
      &rt,
      &client,
      &mut created_pages,
      &plan_slug,
      "План",
      "# План публикации\n\n- Тема: запуск OmniFuse\n- Тезис: быстрый черновик с ачепяткой\n- Проверка фактов: ожидается\n"
    )?;
    create_disposable_wiki_page(
      &rt,
      &client,
      &mut created_pages,
      &article_slug,
      "Статья",
      "# Статья\n\nЧерновик ожидается.\n"
    )?;
    create_disposable_wiki_page(&rt, &client, &mut created_pages, &media_slug, "Посевы", "# Посевы\n")?;
    let telegram_id = create_disposable_wiki_page(
      &rt,
      &client,
      &mut created_pages,
      &telegram_slug,
      "Telegram",
      "# Telegram\n\nЧерновик ожидается.\n"
    )?;
    create_disposable_wiki_page(
      &rt,
      &client,
      &mut created_pages,
      &vk_slug,
      "VK",
      "# VK\n\nЧерновик ожидается.\n"
    )?;

    let pages = EditorialSmokePages {
      plan_slug,
      article_slug,
      telegram_slug,
      vk_slug,
      plan_id,
      telegram_id
    };

    run_real_wiki_editorial_smoke(&of_bin, &config, &client, &rt, &root_slug, &pages)
  })();

  cleanup_wiki_pages(&rt, &client, &created_pages);
  result
}

fn run_real_wiki_mount_smoke(
  of_bin: &Path,
  config: &RealWikiSmokeConfig,
  client: &Client,
  rt: &tokio::runtime::Runtime,
  test_slug: &str,
  api_slug: &str,
  api_page_id: u64
) -> Result<()> {
  let tmp = tempfile::Builder::new()
    .prefix("omnifuse-real-wiki-mount-smoke-")
    .tempdir_in("/tmp")
    .context("create Wiki smoke tempdir under /tmp")?
    .keep();
  let mountpoint = tmp.join("mount").join("wiki");
  fs::create_dir_all(&mountpoint).context("create Wiki mountpoint")?;

  let mut mount = CliMount::spawn_wiki(of_bin, config, test_slug, &mountpoint, &tmp)?;
  let mounted_page = page_path_for_slug(&mountpoint, test_slug);
  let mounted_api_page = page_path_for_slug(&mountpoint, api_slug);

  wait_until("mounted Wiki page becomes readable", SMOKE_TIMEOUT, || {
    mount.ensure_running()?;
    Ok(
      path_is_file_with_timeout(&mounted_page, Duration::from_secs(2))?
        && path_is_file_with_timeout(&mounted_api_page, Duration::from_secs(2))?
    )
  })
  .with_context(|| mount.diagnostics())?;

  let nvim_marker = format!("nvim {}", smoke_marker()?);
  let api_marker = format!("api {}", smoke_marker()?);

  let nvim_stderr_log = tmp.join("headless-nvim.stderr.log");
  let mut nvim = spawn_headless_nvim_append(&mounted_page, &nvim_marker, &nvim_stderr_log)?;
  thread::sleep(Duration::from_millis(300));

  rt.block_on(client.update_page(
    api_page_id,
    Some("OmniFuse concurrent API page"),
    Some(&format!(
      "# API page\n\nUpdated concurrently through API.\n\n{api_marker}\n"
    )),
    false
  ))
  .with_context(|| format!("update disposable Wiki API page {api_slug}"))?;

  wait_process_success(
    &mut nvim,
    Duration::from_secs(45),
    "headless nvim append",
    &nvim_stderr_log
  )?;

  wait_until("nvim marker is pushed to the real Wiki page", SMOKE_TIMEOUT, || {
    mount.ensure_running()?;
    let page = rt
      .block_on(client.get_page_by_slug(test_slug))
      .with_context(|| format!("read Wiki page {test_slug}"))?;
    Ok(
      page
        .content
        .as_deref()
        .is_some_and(|content| content.contains(&nvim_marker))
    )
  })
  .with_context(|| mount.diagnostics())?;

  wait_until("API marker is pulled into mounted Wiki file", SMOKE_TIMEOUT, || {
    mount.ensure_running()?;
    path_contains_with_timeout(&mounted_api_page, &api_marker, Duration::from_secs(2))
  })
  .with_context(|| mount.diagnostics())?;

  mount.stop()?;
  cleanup_smoke_dir(&tmp);
  Ok(())
}

struct EditorialSmokePages {
  plan_slug: String,
  article_slug: String,
  telegram_slug: String,
  vk_slug: String,
  plan_id: u64,
  telegram_id: u64
}

fn run_real_wiki_editorial_smoke(
  of_bin: &Path,
  config: &RealWikiSmokeConfig,
  client: &Client,
  rt: &tokio::runtime::Runtime,
  root_slug: &str,
  pages: &EditorialSmokePages
) -> Result<()> {
  let tmp = tempfile::Builder::new()
    .prefix("omnifuse-real-wiki-editorial-smoke-")
    .tempdir_in("/tmp")
    .context("create Wiki editorial smoke tempdir under /tmp")?
    .keep();
  let mountpoint = tmp.join("mount").join("wiki");
  fs::create_dir_all(&mountpoint).context("create Wiki editorial mountpoint")?;

  let mut mount = CliMount::spawn_wiki(of_bin, config, root_slug, &mountpoint, &tmp)?;
  let mounted_plan = page_path_for_slug(&mountpoint, &pages.plan_slug);
  let mounted_article = page_path_for_slug(&mountpoint, &pages.article_slug);
  let mounted_telegram = page_path_for_slug(&mountpoint, &pages.telegram_slug);
  let mounted_vk = page_path_for_slug(&mountpoint, &pages.vk_slug);

  wait_until("editorial Wiki pages become readable", SMOKE_TIMEOUT, || {
    mount.ensure_running()?;
    Ok(
      path_is_file_with_timeout(&mounted_plan, Duration::from_secs(2))?
        && path_is_file_with_timeout(&mounted_article, Duration::from_secs(2))?
        && path_is_file_with_timeout(&mounted_telegram, Duration::from_secs(2))?
        && path_is_file_with_timeout(&mounted_vk, Duration::from_secs(2))?
    )
  })
  .with_context(|| mount.diagnostics())?;

  rt.block_on(client.update_page(
    pages.plan_id,
    Some("План"),
    Some("# План публикации\n\n- Тема: запуск OmniFuse\n- Тезис: быстрый черновик с ачепяткой\n- Проверка фактов: ожидается\n- Каналы посева: Telegram, VK и email.\n"),
    false
  ))
  .context("append plan section through Wiki API")?;

  wait_until("API plan update is pulled into mounted file", SMOKE_TIMEOUT, || {
    mount.ensure_running()?;
    path_contains_with_timeout(&mounted_plan, "Каналы посева", Duration::from_secs(2))
  })
  .with_context(|| mount.diagnostics())?;

  let plan_stderr_log = tmp.join("headless-nvim-plan.stderr.log");
  let plan_commands = [String::from("%s/с ачепяткой/без опечатки/ge")];
  let mut plan_nvim = spawn_headless_nvim_edit(
    &mounted_plan,
    &plan_commands,
    Duration::from_millis(300),
    &plan_stderr_log
  )?;
  wait_process_success(
    &mut plan_nvim,
    Duration::from_secs(45),
    "headless nvim plan correction",
    &plan_stderr_log
  )?;

  wait_until("corrected plan is visible through Wiki API", SMOKE_TIMEOUT, || {
    mount.ensure_running()?;
    let page = rt.block_on(client.get_page_by_slug(&pages.plan_slug))?;
    let content = page.content.unwrap_or_default();
    if content.contains("без опечатки")
      && content.contains("Каналы посева")
      && !content.contains("<<<<<<<")
      && !content.contains(">>>>>>>")
    {
      Ok(true)
    } else {
      Err(anyhow::anyhow!("current plan content:\n{content}"))
    }
  })
  .with_context(|| mount.diagnostics())?;

  wait_until("corrected plan is visible through mounted file", SMOKE_TIMEOUT, || {
    mount.ensure_running()?;
    Ok(
      path_contains_with_timeout(&mounted_plan, "без опечатки", Duration::from_secs(2))?
        && path_contains_with_timeout(&mounted_plan, "Каналы посева", Duration::from_secs(2))?
    )
  })
  .with_context(|| mount.diagnostics())?;

  let article_stderr_log = tmp.join("headless-nvim-article.stderr.log");
  let mut article_nvim = spawn_headless_nvim_append_lines(
    &mounted_article,
    &[
      "",
      "## Лид",
      "Материал объясняет запуск OmniFuse для публикацыи в нескольких медиа.",
      "",
      "## Основной текст",
      "Редакция сначала согласует план, затем готовит большую статью и короткие посевы.",
      "Важный инвариант: исправления и новые пункты не теряются при параллельной работе.",
      "",
      "## Финал",
      "Эталонный текст должен сойтись у всех участников."
    ],
    Duration::from_millis(300),
    &article_stderr_log
  )?;
  wait_process_success(
    &mut article_nvim,
    Duration::from_secs(45),
    "headless nvim article draft",
    &article_stderr_log
  )?;

  wait_until("article draft is pushed to Wiki API", SMOKE_TIMEOUT, || {
    mount.ensure_running()?;
    let page = rt.block_on(client.get_page_by_slug(&pages.article_slug))?;
    Ok(
      page
        .content
        .as_deref()
        .is_some_and(|content| content.contains("публикацыи") && content.contains("Эталонный текст"))
    )
  })
  .with_context(|| mount.diagnostics())?;

  let article_fix_stderr_log = tmp.join("headless-nvim-article-fix.stderr.log");
  let article_fix_commands = [String::from(
    "%s/публикацыи в нескольких медиа/публикации во внешних медиа/ge"
  )];
  let mut article_fix_nvim = spawn_headless_nvim_edit(
    &mounted_article,
    &article_fix_commands,
    Duration::from_millis(1500),
    &article_fix_stderr_log
  )?;
  thread::sleep(Duration::from_millis(300));

  rt.block_on(client.update_page(
    pages.telegram_id,
    Some("Telegram"),
    Some("# Telegram\n\nКороткий посев: OmniFuse помогает редакции не терять правки.\n\nCTA: читать полную статью.\n"),
    false
  ))
  .context("write Telegram seed through Wiki API")?;
  wait_process_success(
    &mut article_fix_nvim,
    Duration::from_secs(45),
    "headless nvim article correction",
    &article_fix_stderr_log
  )?;

  let vk_marker = format!("vk {}", smoke_marker()?);
  let vk_stderr_log = tmp.join("headless-nvim-vk.stderr.log");
  let mut vk_nvim = spawn_headless_nvim_append_lines(
    &mounted_vk,
    &[
      "",
      "Сокращённая версия: план, статья и посевы синхронизируются между участниками.",
      &vk_marker
    ],
    Duration::from_millis(300),
    &vk_stderr_log
  )?;
  wait_process_success(
    &mut vk_nvim,
    Duration::from_secs(45),
    "headless nvim VK seed",
    &vk_stderr_log
  )?;

  wait_until("final article correction is pushed to Wiki API", SMOKE_TIMEOUT, || {
    mount.ensure_running()?;
    let page = rt.block_on(client.get_page_by_slug(&pages.article_slug))?;
    let content = page.content.unwrap_or_default();
    Ok(content.contains("публикации во внешних медиа") && !content.contains("публикацыи"))
  })
  .with_context(|| mount.diagnostics())?;

  wait_until("Telegram API update is pulled into mounted file", SMOKE_TIMEOUT, || {
    mount.ensure_running()?;
    path_contains_with_timeout(&mounted_telegram, "Короткий посев", Duration::from_secs(2))
  })
  .with_context(|| mount.diagnostics())?;

  wait_until(
    "VK seed written through mount is pushed to Wiki API",
    SMOKE_TIMEOUT,
    || {
      mount.ensure_running()?;
      let page = rt.block_on(client.get_page_by_slug(&pages.vk_slug))?;
      Ok(
        page
          .content
          .as_deref()
          .is_some_and(|content| content.contains(&vk_marker))
      )
    }
  )
  .with_context(|| mount.diagnostics())?;

  let article = rt.block_on(client.get_page_by_slug(&pages.article_slug))?;
  assert!(
    article
      .content
      .as_deref()
      .is_some_and(|content| content.contains("Эталонный текст") && !content.contains("<<<<<<<")),
    "article should keep canonical text without conflict markers"
  );

  mount.stop()?;
  cleanup_smoke_dir(&tmp);
  Ok(())
}

struct RealWikiSmokeConfig {
  base_url: String,
  root_slug: String,
  token: String,
  org_id: Option<String>
}

impl RealWikiSmokeConfig {
  fn from_env() -> Result<Self> {
    Ok(Self {
      base_url: env_or_default("OMNIFUSE_WIKI_URL", "https://wiki-api.yandex-team.ru"),
      root_slug: env_or_default("OMNIFUSE_WIKI_ROOT_SLUG", "veged/test"),
      token: env_required_any(&["OMNIFUSE_WIKI_TOKEN", "YA_WIKI_KEY"])?,
      org_id: std::env::var("OMNIFUSE_WIKI_ORG_ID")
        .ok()
        .filter(|value| !value.trim().is_empty())
    })
  }

  fn test_slug(&self) -> Result<String> {
    let suffix = smoke_marker()?.replace(' ', "-");
    Ok(format!(
      "{}/omnifuse-mount-smoke-{}",
      self.root_slug.trim_matches('/'),
      suffix
    ))
  }

  fn editorial_test_slug(&self) -> Result<String> {
    let suffix = smoke_marker()?.replace(' ', "-");
    Ok(format!(
      "{}/omnifuse-editorial-smoke-{}",
      self.root_slug.trim_matches('/'),
      suffix
    ))
  }
}

struct CliMount {
  child: Child,
  mountpoint: PathBuf,
  stdout_log: PathBuf,
  stderr_log: PathBuf,
  command_name: &'static str,
  stopped: bool
}

impl CliMount {
  fn spawn_git(of_bin: &Path, bare_remote: &Path, mountpoint: &Path, tmp: &Path) -> Result<Self> {
    let stdout_log = tmp.join("of-mount.stdout.log");
    let stderr_log = tmp.join("of-mount.stderr.log");
    let stdout = fs::File::create(&stdout_log).context("create mount stdout log")?;
    let stderr = fs::File::create(&stderr_log).context("create mount stderr log")?;

    let child = Command::new(of_bin)
      .args(["-vv", "mount", "git"])
      .arg(bare_remote)
      .arg(mountpoint)
      .args(["--branch", "main", "--poll-interval", "1"])
      .stdin(Stdio::null())
      .stdout(Stdio::from(stdout))
      .stderr(Stdio::from(stderr))
      .spawn()
      .context("spawn `of mount git`")?;

    Ok(Self {
      child,
      mountpoint: mountpoint.to_path_buf(),
      stdout_log,
      stderr_log,
      command_name: "`of mount git`",
      stopped: false
    })
  }

  fn spawn_wiki(
    of_bin: &Path,
    config: &RealWikiSmokeConfig,
    root_slug: &str,
    mountpoint: &Path,
    tmp: &Path
  ) -> Result<Self> {
    let stdout_log = tmp.join("of-mount.stdout.log");
    let stderr_log = tmp.join("of-mount.stderr.log");
    let stdout = fs::File::create(&stdout_log).context("create mount stdout log")?;
    let stderr = fs::File::create(&stderr_log).context("create mount stderr log")?;

    let mut command = Command::new(of_bin);
    command
      .args(["-vv", "mount", "wiki"])
      .arg(&config.base_url)
      .arg(root_slug)
      .arg(mountpoint)
      .args(["--poll-interval", "1"])
      .env("OMNIFUSE_WIKI_TOKEN", &config.token)
      .stdin(Stdio::null())
      .stdout(Stdio::from(stdout))
      .stderr(Stdio::from(stderr));

    if let Some(org_id) = config.org_id.as_deref() {
      command.env("OMNIFUSE_WIKI_ORG_ID", org_id);
    }

    let child = command.spawn().context("spawn `of mount wiki`")?;

    Ok(Self {
      child,
      mountpoint: mountpoint.to_path_buf(),
      stdout_log,
      stderr_log,
      command_name: "`of mount wiki`",
      stopped: false
    })
  }

  fn ensure_running(&mut self) -> Result<()> {
    if let Some(status) = self.child.try_wait().context("poll mount process")? {
      bail!(
        "{} exited before the smoke test finished: {status}\n{}",
        self.command_name,
        self.diagnostics()
      );
    }

    Ok(())
  }

  fn stop(&mut self) -> Result<()> {
    if self.stopped {
      return Ok(());
    }

    unmount(&self.mountpoint)?;

    let mut status = None;
    wait_until("mount process exits after unmount", Duration::from_secs(10), || {
      status = self.child.try_wait().context("poll mount process after unmount")?;
      Ok(status.is_some())
    })?;

    self.stopped = true;

    let Some(status) = status else {
      bail!("mount process did not report an exit status");
    };

    if !status.success() {
      bail!(
        "{} exited with a non-zero status after unmount: {status}\n{}",
        self.command_name,
        self.diagnostics()
      );
    }

    Ok(())
  }

  fn diagnostics(&self) -> String {
    format!(
      "stdout:\n{}\nstderr:\n{}",
      read_log(&self.stdout_log),
      read_log(&self.stderr_log)
    )
  }
}

impl Drop for CliMount {
  fn drop(&mut self) {
    if self.stopped {
      return;
    }

    let _ = unmount(&self.mountpoint);
    let _ = wait_child(&mut self.child, Duration::from_secs(2));
    if matches!(self.child.try_wait(), Ok(None)) {
      let _ = self.child.kill();
      let _ = self.child.try_wait();
    }
  }
}

fn of_check_passes(of_bin: &Path) -> Result<bool> {
  let output = Command::new(of_bin)
    .arg("check")
    .stdin(Stdio::null())
    .output()
    .context("run `of check`")?;

  Ok(output.status.success())
}

fn env_required_any(names: &[&str]) -> Result<String> {
  for name in names {
    if let Ok(value) = std::env::var(name) {
      if !value.trim().is_empty() {
        return Ok(value);
      }
    }
  }

  bail!("one of {} is required", names.join(", "));
}

fn env_or_default(name: &str, default: &str) -> String {
  std::env::var(name)
    .ok()
    .filter(|value| !value.trim().is_empty())
    .unwrap_or_else(|| default.to_string())
}

fn create_disposable_wiki_page(
  rt: &tokio::runtime::Runtime,
  client: &Client,
  created_pages: &mut Vec<(String, u64)>,
  slug: &str,
  title: &str,
  content: &str
) -> Result<u64> {
  let page = rt
    .block_on(client.create_page(slug, title, Some(content), "page"))
    .with_context(|| format!("create disposable Wiki page {slug}"))?;
  created_pages.push((slug.to_string(), page.id));
  Ok(page.id)
}

fn cleanup_wiki_pages(rt: &tokio::runtime::Runtime, client: &Client, created_pages: &[(String, u64)]) {
  for (slug, id) in created_pages.iter().rev() {
    if let Err(error) = rt.block_on(client.delete_page(*id)) {
      eprintln!("WARN: failed to delete disposable Wiki page {slug}: {error}");
    }
  }
}

fn seed_bare_remote(bare_remote: &Path, seed_clone: &Path) -> Result<()> {
  let mut init = Command::new("git");
  init.args(["init", "--bare", "-b", "main"]).arg(bare_remote);
  run(init, "git init --bare")?;

  clone_remote(bare_remote, seed_clone)?;

  let mut email = Command::new("git");
  email
    .current_dir(seed_clone)
    .args(["config", "user.email", "smoke@omnifuse.test"]);
  run(email, "git config user.email")?;

  let mut name = Command::new("git");
  name
    .current_dir(seed_clone)
    .args(["config", "user.name", "OmniFuse Smoke"]);
  run(name, "git config user.name")?;

  fs::write(seed_clone.join("README.md"), "# smoke repo\n").context("write seed README.md")?;

  let mut add = Command::new("git");
  add.current_dir(seed_clone).args(["add", "README.md"]);
  run(add, "git add README.md")?;

  let mut commit = Command::new("git");
  commit.current_dir(seed_clone).args(["commit", "-m", "init"]);
  run(commit, "git commit init")?;

  let mut push = Command::new("git");
  push.current_dir(seed_clone).args(["push", "-u", "origin", "main"]);
  run(push, "git push origin main")?;

  Ok(())
}

fn clone_remote(bare_remote: &Path, target: &Path) -> Result<()> {
  let mut clone = Command::new("git");
  clone.args(["clone"]).arg(bare_remote).arg(target);
  run(clone, "git clone")?;
  Ok(())
}

fn git_show(bare_remote: &Path, spec: &str) -> Result<String> {
  let mut show = Command::new("git");
  show.arg("--git-dir").arg(bare_remote).args(["show", spec]);
  let output = run(show, "git show")?;
  Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

fn append_marker(path: &Path, marker: &str) -> Result<()> {
  let mut child = Command::new("tee")
    .arg("-a")
    .arg(path)
    .stdin(Stdio::piped())
    .stdout(Stdio::null())
    .stderr(Stdio::piped())
    .spawn()
    .with_context(|| format!("spawn tee append for {}", path.display()))?;

  if let Some(mut stdin) = child.stdin.take() {
    writeln!(stdin, "\n{marker}").context("write smoke marker to tee")?;
  } else {
    bail!("tee stdin is unavailable");
  }

  if wait_child(&mut child, Duration::from_secs(5))?.is_none() {
    let _ = child.kill();
    bail!("append through mountpoint timed out");
  }

  let output = child.wait_with_output().context("collect tee output")?;
  if !output.status.success() {
    bail!("append through mountpoint failed: {}", output_summary(&output));
  }

  Ok(())
}

fn spawn_headless_nvim_append(path: &Path, marker: &str, stderr_log: &Path) -> Result<Child> {
  spawn_headless_nvim_append_lines(path, &["", marker], Duration::from_millis(1500), stderr_log)
}

fn spawn_headless_nvim_append_lines(
  path: &Path,
  lines: &[&str],
  sleep_before_write: Duration,
  stderr_log: &Path
) -> Result<Child> {
  let quoted_lines = lines
    .iter()
    .map(|line| format!("'{}'", vim_single_quoted(line)))
    .collect::<Vec<_>>()
    .join(", ");
  let commands = [format!("call append(line('$'), [{quoted_lines}])")];
  spawn_headless_nvim_edit(path, &commands, sleep_before_write, stderr_log)
}

fn spawn_headless_nvim_edit(
  path: &Path,
  commands: &[String],
  sleep_before_write: Duration,
  stderr_log: &Path
) -> Result<Child> {
  let stderr = fs::File::create(stderr_log).context("create nvim stderr log")?;
  let mut command = Command::new("nvim");
  command
    .args(["--headless", "--clean", "-n"])
    .arg(path)
    .arg("+set nomore noswapfile nobackup nowritebackup backupcopy=yes backupskip=*");

  for edit_command in commands {
    command.arg(format!("+{edit_command}"));
  }

  if !sleep_before_write.is_zero() {
    command.arg(format!("+sleep {}m", sleep_before_write.as_millis()));
  }

  command
    .arg("+write!")
    .arg("+if v:errmsg != '' | cquit | endif")
    .arg("+quitall!")
    .stdin(Stdio::null())
    .stdout(Stdio::null())
    .stderr(Stdio::from(stderr))
    .spawn()
    .with_context(|| format!("spawn headless nvim for {}", path.display()))
}

fn vim_single_quoted(value: &str) -> String {
  value.replace('\\', "\\\\").replace('\'', "''")
}

fn smoke_marker() -> Result<String> {
  let ts = SystemTime::now()
    .duration_since(UNIX_EPOCH)
    .context("system time before UNIX_EPOCH")?
    .as_nanos();
  Ok(format!("smoke {ts}"))
}

fn page_path_for_slug(mountpoint: &Path, slug: &str) -> PathBuf {
  let mut path = mountpoint.to_path_buf();
  for segment in slug.split('/') {
    path.push(segment);
  }
  path.set_extension("md");
  path
}

fn wait_until(label: &str, timeout: Duration, mut predicate: impl FnMut() -> Result<bool>) -> Result<()> {
  let start = Instant::now();
  let mut last_error = None;

  while start.elapsed() < timeout {
    match predicate() {
      Ok(true) => return Ok(()),
      Ok(false) => {}
      Err(error) => last_error = Some(error.to_string())
    }

    thread::sleep(POLL_INTERVAL);
  }

  if let Some(error) = last_error {
    bail!("timed out while waiting for {label}; last error: {error}");
  }

  bail!("timed out while waiting for {label}");
}

fn unmount(mountpoint: &Path) -> Result<()> {
  let mut errors = Vec::new();

  for mut command in unmount_commands(mountpoint) {
    let label = format!("{command:?}");
    match run_unmount_command(&mut command, Duration::from_secs(5)) {
      Ok(()) => return Ok(()),
      Err(error) => errors.push(format!("{label}: {error}"))
    }
  }

  bail!("failed to unmount {}:\n{}", mountpoint.display(), errors.join("\n"));
}

fn path_is_file_with_timeout(path: &Path, timeout: Duration) -> Result<bool> {
  let mut child = Command::new("test")
    .arg("-f")
    .arg(path)
    .stdin(Stdio::null())
    .stdout(Stdio::null())
    .stderr(Stdio::null())
    .spawn()
    .with_context(|| format!("spawn test -f for {}", path.display()))?;

  wait_child(&mut child, timeout)?.map_or_else(
    || {
      let _ = child.kill();
      let _ = child.try_wait();
      Ok(false)
    },
    |status| Ok(status.success())
  )
}

fn path_contains_with_timeout(path: &Path, needle: &str, timeout: Duration) -> Result<bool> {
  let mut child = Command::new("grep")
    .args(["-F", "--", needle])
    .arg(path)
    .stdin(Stdio::null())
    .stdout(Stdio::null())
    .stderr(Stdio::null())
    .spawn()
    .with_context(|| format!("spawn grep for {}", path.display()))?;

  wait_child(&mut child, timeout)?.map_or_else(
    || {
      let _ = child.kill();
      let _ = child.try_wait();
      Ok(false)
    },
    |status| Ok(status.success())
  )
}

#[cfg(target_os = "macos")]
fn unmount_commands(mountpoint: &Path) -> Vec<Command> {
  let mut umount = Command::new("umount");
  umount.arg(mountpoint);

  let mut diskutil = Command::new("diskutil");
  diskutil.args(["unmount", "force"]).arg(mountpoint);

  vec![umount, diskutil]
}

#[cfg(target_os = "linux")]
fn unmount_commands(mountpoint: &Path) -> Vec<Command> {
  let mut fusermount3 = Command::new("fusermount3");
  fusermount3.args(["-u"]).arg(mountpoint);

  let mut fusermount = Command::new("fusermount");
  fusermount.args(["-u"]).arg(mountpoint);

  let mut umount = Command::new("umount");
  umount.arg(mountpoint);

  vec![fusermount3, fusermount, umount]
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn unmount_commands(mountpoint: &Path) -> Vec<Command> {
  let mut umount = Command::new("umount");
  umount.arg(mountpoint);
  vec![umount]
}

fn wait_child(child: &mut Child, timeout: Duration) -> Result<Option<ExitStatus>> {
  let start = Instant::now();
  while start.elapsed() < timeout {
    if let Some(status) = child.try_wait().context("poll child process")? {
      return Ok(Some(status));
    }

    thread::sleep(POLL_INTERVAL);
  }

  Ok(None)
}

fn wait_process_success(child: &mut Child, timeout: Duration, label: &str, stderr_log: &Path) -> Result<()> {
  let Some(status) = wait_child(child, timeout)? else {
    let _ = child.kill();
    bail!("{label} timed out after {timeout:?}\nstderr:\n{}", read_log(stderr_log));
  };

  if !status.success() {
    bail!("{label} exited with {status}\nstderr:\n{}", read_log(stderr_log));
  }

  Ok(())
}

fn run_unmount_command(command: &mut Command, timeout: Duration) -> Result<()> {
  let mut child = command
    .stdin(Stdio::null())
    .stdout(Stdio::null())
    .stderr(Stdio::null())
    .spawn()
    .context("spawn unmount command")?;

  if wait_child(&mut child, timeout)?.is_none() {
    let _ = child.kill();
    bail!("unmount command timed out after {timeout:?}");
  }

  let status = child.wait().context("collect unmount command status")?;
  if !status.success() {
    bail!("unmount command exited with {status}");
  }

  Ok(())
}

fn run(mut command: Command, label: &str) -> Result<Output> {
  let output = command
    .stdin(Stdio::null())
    .output()
    .with_context(|| format!("run {label}"))?;

  if !output.status.success() {
    bail!("{label} failed: {}", output_summary(&output));
  }

  Ok(output)
}

fn output_summary(output: &Output) -> String {
  format!(
    "status: {}\nstdout:\n{}\nstderr:\n{}",
    output.status,
    String::from_utf8_lossy(&output.stdout),
    String::from_utf8_lossy(&output.stderr)
  )
}

fn read_log(path: &Path) -> String {
  fs::read_to_string(path).unwrap_or_else(|error| format!("<failed to read {}: {error}>", path.display()))
}

fn cleanup_smoke_dir(path: &Path) {
  if let Err(error) = fs::remove_dir_all(path) {
    eprintln!("WARN: failed to remove smoke dir {}: {error}", path.display());
  }
}
