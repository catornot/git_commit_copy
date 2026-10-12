use clap::{CommandFactory, Parser, Subcommand, ValueHint};
use clap_complete::{Shell, generate};
use git2::{Commit, Oid, Repository, Signature, Time, Tree, build::CheckoutBuilder};
use miette::{Context, IntoDiagnostic, Result};
use regex::Regex;
use std::{
    collections::HashMap,
    io,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

pub struct CommitBuilder<'a> {
    author: Signature<'a>,
    committer: Signature<'a>,
    time: Time,
    message: Box<str>,
    tree: Tree<'a>,
    commit: Commit<'a>,
    oid: Oid,
}

#[derive(Parser, Debug)]
#[clap(name = "cat_or_not")]
#[clap(author = "@cat_or_not:matrix.catornot.net")]
#[clap(
    about = "allows you to copy commits from one git repo to another, use the current checkout branch on the destination repo"
)]
#[clap(after_help = "Hi")]
#[command(name = "git_commit_copy", version, long_about = None)]
pub struct Args {
    #[arg(value_name = "list of commits", value_hint = ValueHint::Url)]
    commits: Vec<String>,

    #[clap(short, long, value_name = "source repo")]
    #[arg(help = "the path to source repo on the file system")]
    src: PathBuf,

    #[clap(short, long, value_name = "destination repo")]
    #[arg(help = "the path to destination repo on the file system")]
    dst: PathBuf,

    #[clap(short, long, value_name = "prefix path for the commits")]
    #[arg(help = "the prefix that will appended to the path of the copied commits")]
    prefix: Option<String>,

    #[clap(short, long, value_name = "prefix path for the commits")]
    #[arg(
        help = "the \"path\" on github to get to the src repo to rewrite squash commits to point to the original prs"
    )]
    github_path: Option<String>,

    #[clap(long, value_name = "set time")]
    #[arg(
        help = "the branch to witch the commit will be attached",
        default_value_t = false
    )]
    fix_date: bool,

    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand, Debug)]
enum Commands {
    Completions {
        #[arg(value_enum)]
        shell: Shell,
    },
}

fn main() -> Result<()> {
    let args = Args::parse();

    match args.command {
        Some(Commands::Completions { shell }) => {
            generate(
                shell,
                &mut Args::command(),
                "git_commit_copy",
                &mut io::stdout(),
            );
            return Ok(());
        }
        None => {}
    }

    let src = Repository::open(&args.src)
        .into_diagnostic()
        .context("failed to open src repo")?;
    let dst = Repository::open(&args.dst)
        .into_diagnostic()
        .context("failed to open dst repo")?;
    let commits = args
        .commits
        .into_iter()
        .map(|commit| Oid::from_str(&commit))
        .collect::<Result<Vec<Oid>, _>>()
        .into_diagnostic()
        .context("failed to get all commits")?;

    _ = import_objects(
        &dst,
        &src,
        &args
            .src
            .components()
            .next_back()
            .ok_or(miette::Error::msg("couldn't find a good name for src repo"))?
            .as_os_str()
            .to_string_lossy(),
    );

    migrate_commits(
        &dst,
        &src,
        &commits,
        args.prefix.as_deref(),
        args.github_path.as_deref(),
        args.fix_date,
    )?;

    Ok(())
}

fn migrate_commits(
    dst: &Repository,
    src: &Repository,
    commits: &[Oid],
    prefix: Option<&str>,
    github_path: Option<&str>,
    new_time: bool,
) -> Result<(), miette::Report> {
    let re = Regex::new(r"\(#\d+\)").into_diagnostic()?;
    let now = new_time.then(|| {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("Time went backwards")
            .as_secs() as i64
    });

    let commits_pool = commits
        .iter()
        .cloned()
        .enumerate()
        .map(|(i, oid)| {
            let commit = src.find_commit(oid).into_diagnostic()?;

            let mut message = commit.message().into_diagnostic()?.to_string();

            while let Some((number, github_path)) = github_path.and_then(|github_path| {
                Some((
                    re.find(&message)
                        .map(|r| r.start() + 2..r.end() - 1)
                        .and_then(|r| message.get(r)?.parse::<u32>().ok())?,
                    github_path,
                ))
            }) {
                message = re
                    .replacen(&message, 1, format!("({}#{})", github_path, number))
                    .to_string();
            }
            let tree = commit.tree().into_diagnostic()?;

            let commit = CommitBuilder {
                author: commit.author().to_owned(),
                committer: commit.committer().to_owned(),
                time: now
                    .map(|seconds| Time::new(seconds + i as i64, 0))
                    .unwrap_or_else(|| commit.time()),
                message: message.into(),
                tree,
                commit: commit.clone(),
                oid,
            };

            eprintln!("{} {:?}", oid, commit.message);

            Ok(commit)
        })
        .collect::<Result<Vec<CommitBuilder<'_>>, miette::Error>>()?;

    let head = dst
        .head()
        .into_diagnostic()
        .context("no head for this branch")?;

    let parent = head
        .peel_to_commit()
        .into_diagnostic()
        .context("no head commit this branch")?;

    let mut rewritten = HashMap::<Oid, Oid>::new();
    let new_head = commits_pool
        .into_iter()
        .try_fold(parent.clone(), |parent, commit| {
            let new_parents = commit
                .commit
                .parent_ids()
                .filter_map(|old_parent| rewritten.get(&old_parent).copied())
                .map(|oid| dst.find_commit(oid))
                .collect::<Result<Vec<_>, _>>()?;

            let new_parents = if new_parents.is_empty() {
                vec![parent.clone()]
            } else {
                new_parents
            };

            // Current destination tree.
            let base_tree = parent.tree()?;

            let tree_oid = mk_new_tree(dst, prefix, &commit, base_tree)?;
            let tree = dst.find_tree(tree_oid)?;

            let committer = Signature::new(
                commit.committer.name()?,
                commit.committer.email()?,
                &commit.time,
            )?;

            let new_commit = mk_new_commmit(
                dst,
                &new_parents.iter().collect::<Vec<_>>(),
                &commit.author,
                &committer,
                &commit.message,
                tree,
            )?;

            rewritten.insert(commit.oid, new_commit);

            let new_commit = dst.find_commit(new_commit)?;

            eprintln!("moved {}", commit.message);

            Ok::<_, git2::Error>(new_commit)
        })
        .into_diagnostic()?;

    let name = head.name().into_diagnostic()?;
    dst.reference(name, new_head.id(), true, "update imported history")
        .into_diagnostic()?;

    dst.set_head(name).into_diagnostic()?;

    dst.checkout_head(Some(CheckoutBuilder::new().safe().force()))
        .into_diagnostic()?;

    // eprintln!("old commits to new commits map:");
    // rewritten
    //     .into_iter()
    //     .for_each(|(old, new)| println!("{old}->{new}"));

    Ok(())
}

fn mk_new_commmit(
    destination: &Repository,
    new_parents: &[&Commit<'_>],
    author: &Signature<'_>,
    committer: &Signature<'_>,
    message: &str,
    tree: Tree<'_>,
) -> Result<Oid, git2::Error> {
    destination.commit(None, author, committer, message, &tree, new_parents)
}

fn mk_new_tree(
    destination: &Repository,
    prefix: Option<&str>,
    commit: &CommitBuilder<'_>,
    base_tree: Tree<'_>,
) -> Result<Oid, git2::Error> {
    let source_tree = destination.find_tree(commit.commit.tree_id())?;

    let ancestor = if commit.commit.parent_count() > 0 {
        let source_parent = destination.find_commit(commit.commit.parent_id(0)?)?;
        source_parent.tree()?
    } else {
        let empty_oid = destination.treebuilder(None)?.write()?;
        destination.find_tree(empty_oid)?
    };

    if let Some(prefix) = prefix {
        // Merge inside the prefixed directory.
        let existing_tree = base_tree
            .get_path(Path::new(prefix))
            .ok()
            .and_then(|entry| destination.find_tree(entry.id()).ok());

        let empty_oid = destination.treebuilder(None)?.write()?;
        let existing_tree = match existing_tree {
            Some(tree) => tree,
            None => destination.find_tree(empty_oid)?,
        };

        let mut index = destination.merge_trees(&ancestor, &existing_tree, &source_tree, None)?;

        if index.has_conflicts() {
            return Err(git2::Error::from_str(
                "conflicts while merging imported commit",
            ));
        }

        let merged_oid = index.write_tree_to(destination)?;

        let mut builder = destination.treebuilder(Some(&base_tree))?;
        builder.insert(prefix, merged_oid, 0o040000)?;
        builder.write()
    } else {
        let mut index = destination.merge_trees(&ancestor, &base_tree, &source_tree, None)?;

        if index.has_conflicts() {
            return Err(git2::Error::from_str(
                "conflicts while merging imported commit",
            ));
        }

        index.write_tree_to(destination)
    }
}

fn import_objects(
    destination: &Repository,
    source: &Repository,
    name: &str,
) -> Result<(), git2::Error> {
    let url = source
        .path()
        .to_str()
        .ok_or_else(|| git2::Error::from_str("invalid source path"))?;

    let remote_name = format!("import-{name}");

    destination.remote(&remote_name, url)?;

    let mut remote = destination.find_remote(&remote_name)?;

    let refspec = format!("refs/heads/*:refs/import/{name}/*");

    remote.fetch(&[&refspec], None, None)?;

    Ok(())
}
