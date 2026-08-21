#!/usr/bin/env ruby

require "minitest/autorun"
require "stringio"
require "tmpdir"

load File.expand_path("../bin/rz", __dir__)

class RzTest < Minitest::Test
  class RecordingRz < Rz
    attr_reader :commands

    def initialize(argv)
      super
      @commands = []
    end

    private

    def run_command(*command)
      @commands << command
      return "1.3.1" if command.last.include?("get version")
      return "window-current" if command.last.include?("id of first window")

      header = %w[
        window_index window_id window_name window_x window_y window_width window_height
        tab_index tab_id tab_name tab_selected terminal_index terminal_id terminal_name
        terminal_focused working_directory scrollback_path
      ]
      row = [
        "2", "window-current", "Work", "100", "120", "1200", "800",
        "1", "tab-1", "Project", "true", "1", "terminal-1", "codex",
        "true", "/tmp/project", ""
      ]
      ([header.join("\t"), row.join("\t")] + [""]).join("\n")
    end
  end

  def setup
    @state_dir = Dir.mktmpdir("rz-test")
    @previous_state_dir = ENV["RZ_STATE_DIR"]
    ENV["RZ_STATE_DIR"] = @state_dir
    @rz = RecordingRz.new([])
  end

  def teardown
    ENV["RZ_STATE_DIR"] = @previous_state_dir
    FileUtils.remove_entry(@state_dir)
  end

  def test_save_arguments_support_fast_current_window_snapshots
    options = @rz.send(
      :parse_save_arguments,
      ["backup", "--no-scrollback", "--current-window"]
    )

    assert_equal "backup", options.fetch(:name)
    refute options.fetch(:capture_scrollback)
    assert options.fetch(:current_window)
    assert_nil options.fetch(:window_id)
  end

  def test_save_arguments_reject_two_window_selectors
    error = assert_raises(RuntimeError) do
      @rz.send(
        :parse_save_arguments,
        ["backup", "--current-window", "--window-id", "window-1"]
      )
    end

    assert_match(/either --current-window or --window-id/, error.message)
  end

  def test_restore_replaces_existing_windows_by_default
    options = @rz.send(:parse_restore_arguments, ["--session", "work"])

    assert_equal "work", options.fetch(:selector)
    refute options.fetch(:dry_run)
    assert options.fetch(:close_existing)
  end

  def test_restore_can_keep_existing_windows
    options = @rz.send(
      :parse_restore_arguments,
      ["--keep-existing", "--session", "work", "--dry-run"]
    )

    assert_equal "work", options.fetch(:selector)
    assert options.fetch(:dry_run)
    refute options.fetch(:close_existing)
  end

  def test_list_arguments_accept_one_positive_snapshot_number
    assert_nil @rz.send(:parse_list_arguments, [])
    assert_equal 12, @rz.send(:parse_list_arguments, ["12"])

    error = assert_raises(RuntimeError) do
      @rz.send(:parse_list_arguments, ["0"])
    end
    assert_match(/number shown by rz --list/, error.message)
  end

  def test_list_number_shows_details_for_the_corresponding_row
    snapshots_dir = File.join(@state_dir, "snapshots")
    snapshot = lambda do |id, saved_at, title|
      {
        "version" => 1,
        "name" => "work",
        "id" => id,
        "saved_at" => saved_at,
        "timestamp" => id.split("_", 2).last,
        "ghostty_version" => "1.3.1",
        "tabs_count" => 1,
        "terminals_count" => 1,
        "scrollback_captured" => true,
        "scope" => {"type" => "all_windows"},
        "windows" => [
          {
            "index" => 1,
            "name" => title,
            "position" => {"x" => 100, "y" => 120},
            "size" => {"width" => 1200, "height" => 800},
            "tabs" => [
              {
                "index" => 1,
                "name" => "Project",
                "selected" => true,
                "terminals" => [
                  {
                    "index" => 1,
                    "name" => "codex",
                    "focused" => true,
                    "working_directory" => "/tmp/project",
                    "scrollback_file" => "scrollback/terminal.txt",
                    "codex_session_id" => "019f7eb7-dc72-75b3-b042-91599cdd90ac"
                  }
                ]
              }
            ]
          }
        ],
        "warnings" => []
      }
    end
    states = [
      snapshot.call("work_20260720-120000", "2026-07-20T12:00:00+03:00", "Older window"),
      snapshot.call("work_20260721-120000", "2026-07-21T12:00:00+03:00", "Newer window")
    ]
    states.each do |state|
      directory = File.join(snapshots_dir, state.fetch("id"))
      FileUtils.mkdir_p(directory)
      File.write(File.join(directory, "state.json"), JSON.generate(state))
    end

    output, = capture_io { RecordingRz.new(["--list", "2"]).run }

    assert_match(/NUMBER\s+02/, output)
    assert_includes output, "work_20260720-120000"
    assert_includes output, "WINDOW       1  Older window  |  1200x800 at 100,120"
    assert_includes output, "TAB          1.1  Project  |  1 terminal(s)  |  selected"
    assert_includes output, "TERM         1.1.1  codex  |  focused, scrollback"
    assert_includes output, "/tmp/project"
    assert_includes output, "019f7eb7-dc72-75b3-b042-91599cdd90ac"
    refute_includes output, "Newer window"

    error = assert_raises(RuntimeError) { @rz.send(:list, 3) }
    assert_match(/choose 1-2 from rz --list/, error.message)
  end

  def test_watcher_defaults_to_the_current_window
    options = @rz.send(:parse_watch_arguments, ["backup", "--every", "15m"])

    assert_equal "backup", options.fetch(:name)
    assert_equal 900, options.fetch(:interval_seconds)
    assert_equal "current-window", options.fetch(:scope)
  end

  def test_watcher_can_explicitly_cover_all_windows
    options = @rz.send(
      :parse_watch_arguments,
      ["backup", "--every", "1h", "--all-windows"]
    )

    assert_equal 3600, options.fetch(:interval_seconds)
    assert_equal "all-windows", options.fetch(:scope)
  end

  def test_watcher_rejects_an_aggressive_interval
    error = assert_raises(RuntimeError) do
      @rz.send(:parse_watch_arguments, ["backup", "--every", "5s"])
    end

    assert_match(/at least 10 seconds/, error.message)
  end

  def test_clean_defaults_to_seven_days_and_accepts_friendly_ages
    assert_equal 7 * 24 * 60 * 60, @rz.send(:parse_clean_arguments, []).fetch(:age_seconds)
    assert_equal 2 * 24 * 60 * 60, @rz.send(:parse_clean_arguments, ["2.days"]).fetch(:age_seconds)
    assert_equal 14 * 24 * 60 * 60, @rz.send(:parse_clean_arguments, ["2weeks"]).fetch(:age_seconds)
    assert_equal 12 * 60 * 60, @rz.send(:parse_clean_arguments, ["12h"]).fetch(:age_seconds)
  end

  def test_clean_rejects_invalid_or_multiple_ages
    error = assert_raises(RuntimeError) do
      @rz.send(:parse_clean_arguments, ["last-week"])
    end
    assert_match(/invalid cleanup age/, error.message)

    error = assert_raises(RuntimeError) do
      @rz.send(:parse_clean_arguments, ["7d", "14d"])
    end
    assert_match(/single age/, error.message)
  end

  def test_clean_removes_only_snapshots_older_than_the_requested_age
    snapshots_dir = File.join(@state_dir, "snapshots")
    old_dir = File.join(snapshots_dir, "old_20260701-120000")
    recent_dir = File.join(snapshots_dir, "recent_20260720-120000")
    FileUtils.mkdir_p(old_dir)
    FileUtils.mkdir_p(recent_dir)
    File.write(
      File.join(old_dir, "state.json"),
      JSON.generate("id" => "old_20260701-120000", "saved_at" => (Time.now - (8 * 24 * 60 * 60)).iso8601)
    )
    File.write(
      File.join(recent_dir, "state.json"),
      JSON.generate("id" => "recent_20260720-120000", "saved_at" => (Time.now - (6 * 24 * 60 * 60)).iso8601)
    )

    output, = capture_io do
      @rz.send(:clean, age_seconds: 7 * 24 * 60 * 60)
    end

    refute Dir.exist?(old_dir)
    assert Dir.exist?(recent_dir)
    assert_includes output, "old_20260701-120000"
    assert_includes output, "7 days"
    assert_match(/REMOVED\s+1 snapshot/, output)
    assert_match(/KEPT\s+1 newer snapshot/, output)
  end

  def test_clean_refuses_to_follow_a_symlinked_snapshot_directory
    snapshots_dir = File.join(@state_dir, "snapshots")
    external_dir = Dir.mktmpdir("rz-external-snapshot")
    FileUtils.mkdir_p(snapshots_dir)
    File.write(
      File.join(external_dir, "state.json"),
      JSON.generate("id" => "external", "saved_at" => (Time.now - (30 * 24 * 60 * 60)).iso8601)
    )
    link_path = File.join(snapshots_dir, "external")
    File.symlink(external_dir, link_path)

    output, = capture_io do
      @rz.send(:clean, age_seconds: 7 * 24 * 60 * 60)
    end

    assert File.symlink?(link_path)
    assert File.exist?(File.join(external_dir, "state.json"))
    assert_includes output, "refusing to remove unsafe snapshot path"
  ensure
    FileUtils.remove_entry(external_dir) if external_dir && Dir.exist?(external_dir)
  end

  def test_capture_passes_the_bound_window_id_to_applescript
    rows = @rz.send(
      :capture_ghostty,
      capture_scrollback: false,
      window_id: "window-current"
    )

    command = @rz.commands.last
    assert_includes command, "--no-scrollback"
    assert_equal ["--window-id", "window-current"], command.last(2)
    assert_equal 1, rows.length
    assert_equal "window-current", rows.first.fetch("window_id")
    assert_equal 2, rows.first.fetch("window_index")
  end

  def test_snapshot_records_its_window_scope
    rows = @rz.send(
      :capture_ghostty,
      capture_scrollback: false,
      window_id: "window-current"
    )
    snapshot = @rz.send(
      :build_snapshot,
      "backup",
      "backup_20260720-120000",
      "20260720-120000",
      rows,
      [],
      [],
      capture_scrollback: false,
      target_window_id: "window-current"
    )

    assert_equal(
      {"type" => "window", "window_id" => "window-current"},
      snapshot.fetch("scope")
    )
    assert_equal 1, snapshot.fetch("windows").length
    assert_equal 1, snapshot.fetch("terminals_count")
  end

  def test_snapshot_records_the_amp_thread_on_its_terminal
    rows = @rz.send(
      :capture_ghostty,
      capture_scrollback: false,
      window_id: "window-current"
    )
    amp = {
      "agent" => "amp",
      "pid" => 400,
      "tty" => "ttys016",
      "cwd" => "/tmp/project",
      "session_id" => "T-01a023e2-3f9d-7705-98ed-4ea63108e87e"
    }
    rows.first["agent_session"] = amp

    snapshot = @rz.send(
      :build_snapshot,
      "backup",
      "backup_20260720-120000",
      "20260720-120000",
      rows,
      [amp],
      [],
      capture_scrollback: false,
      target_window_id: "window-current"
    )
    terminal = snapshot.fetch("windows").first.fetch("tabs").first.fetch("terminals").first

    assert_equal amp.fetch("session_id"), terminal.fetch("amp_thread_id")
    refute terminal.key?("codex_session_id")
    assert_equal [amp], snapshot.fetch("detected_amp_threads")
    assert_empty snapshot.fetch("detected_codex_sessions")
  end

  def test_save_output_lists_the_saved_tab_name
    output, = capture_io do
      @rz.send(
        :save,
        name: "backup",
        capture_scrollback: false,
        current_window: true
      )
    end

    assert_match(/TAB 2\.1\s+Project/, output)
  end

  def test_saved_tab_lines_mark_empty_titles_as_untitled
    snapshot = {
      "windows" => [
        {
          "index" => 1,
          "tabs" => [
            {"index" => 1, "name" => "Work"},
            {"index" => 2, "name" => ""}
          ]
        }
      ]
    }

    lines = @rz.send(:saved_tab_lines, snapshot)

    assert_match(/TAB 1\.1\s+Work/, lines.first)
    assert_match(/TAB 1\.2\s+\(untitled\)/, lines.last)
  end

  def test_boxes_use_rounded_corners_and_fit_the_terminal_width
    output = StringIO.new
    output.define_singleton_method(:tty?) { true }
    output.define_singleton_method(:winsize) { [24, 40] }

    @rz.send(:print_box, "TEST", ["A line long enough to wrap inside the narrow terminal window"], io: output)

    lines = output.string.lines(chomp: true)
    assert_equal "╭#{"─" * 38}╮", lines.first
    assert_equal "├#{"─" * 38}┤", lines[2]
    assert_equal "╰#{"─" * 38}╯", lines.last
    assert lines.all? { |line| line.length == 40 }
  end

  def test_boxes_keep_the_readable_maximum_in_a_wide_terminal
    output = StringIO.new
    output.define_singleton_method(:tty?) { true }
    output.define_singleton_method(:winsize) { [24, 140] }

    @rz.send(:print_box, "TEST", ["content"], io: output)

    assert output.string.lines(chomp: true).all? { |line| line.length == 92 }
  end

  def test_boxes_wrap_the_heading_in_a_very_narrow_terminal
    output = StringIO.new
    output.define_singleton_method(:tty?) { true }
    output.define_singleton_method(:winsize) { [24, 16] }

    @rz.send(:print_box, "WORKSPACE RESTORED", ["content"], io: output)

    assert output.string.lines(chomp: true).all? { |line| line.length == 16 }
  end

  def test_current_window_id_uses_ghosttys_front_window
    assert_equal "window-current", @rz.send(:current_ghostty_window_id)
    assert_includes @rz.commands.last, "-e"
    assert_includes @rz.commands.last.last, "id of first window"
  end

  def test_extracts_one_open_amp_thread_from_lsof_output
    thread_id = "T-01a023e2-3f9d-7705-98ed-4ea63108e87e"
    output = <<~TEXT
      p19089
      n/opt/homebrew/bin/amp
      n/Users/marian/.cache/amp/logs/threads/#{thread_id}.log
    TEXT

    assert_equal thread_id, @rz.send(:amp_thread_id_from_lsof, output)

    second_id = "T-01a0263b-7c89-7367-9e24-c78e3c0daedc"
    ambiguous = output + "n/Users/marian/.cache/amp/logs/threads/#{second_id}.log\n"
    assert_nil @rz.send(:amp_thread_id_from_lsof, ambiguous)
  end

  def test_process_discovery_only_inspects_supported_agents
    thread_id = "T-01a023e2-3f9d-7705-98ed-4ea63108e87e"
    ps_output = <<~TEXT
          100     1 ??       /Applications/Ghostty.app/Contents/MacOS/ghostty
          200   100 ttys016  /bin/zsh -l
          300   200 ttys016  amp
          400   200 ttys016  /usr/bin/sleep 60
    TEXT
    inspected_pids = []
    rz = Rz.new([])
    rz.define_singleton_method(:run_command) { |*| ps_output }
    rz.define_singleton_method(:process_cwd) do |pid|
      inspected_pids << pid
      "/Users/marian/dot-files"
    end
    rz.define_singleton_method(:open_amp_thread_id) { |_| thread_id }

    sessions = rz.send(:running_agent_sessions)

    assert_equal [300], inspected_pids
    assert_equal 1, sessions.length
    assert_equal "amp", sessions.first.fetch("agent")
    assert_equal thread_id, sessions.first.fetch("session_id")
  end

  def test_matches_amp_and_codex_to_their_own_terminals
    cwd = "/Users/marian/dot-files"
    rows = [
      {
        "terminal_id" => "terminal-amp",
        "terminal_name" => "⣒ macOS display detection - amp - ~/dot-files",
        "working_directory" => cwd
      },
      {
        "terminal_id" => "terminal-codex",
        "terminal_name" => "codex | dot-files",
        "working_directory" => cwd
      }
    ]
    codex = {
      "agent" => "codex",
      "pid" => 300,
      "tty" => "ttys004",
      "cwd" => cwd,
      "session_id" => "019f7eb7-dc72-75b3-b042-91599cdd90ac"
    }
    amp = {
      "agent" => "amp",
      "pid" => 400,
      "tty" => "ttys016",
      "cwd" => cwd,
      "session_id" => "T-01a023e2-3f9d-7705-98ed-4ea63108e87e"
    }

    assignments, warnings = @rz.send(:assign_agent_sessions, rows, [codex, amp])

    assert_equal amp, assignments.fetch("terminal-amp")
    assert_equal codex, assignments.fetch("terminal-codex")
    assert_empty warnings
  end

  def test_does_not_cross_match_agents_with_unidentified_titles_in_the_same_directory
    cwd = "/Users/marian/dot-files"
    rows = [
      {"terminal_id" => "terminal-1", "terminal_name" => "shell one", "working_directory" => cwd},
      {"terminal_id" => "terminal-2", "terminal_name" => "shell two", "working_directory" => cwd}
    ]
    codex = {
      "agent" => "codex",
      "pid" => 300,
      "tty" => "ttys004",
      "cwd" => cwd,
      "session_id" => "019f7eb7-dc72-75b3-b042-91599cdd90ac"
    }
    amp = {
      "agent" => "amp",
      "pid" => 400,
      "tty" => "ttys016",
      "cwd" => cwd,
      "session_id" => "T-01a023e2-3f9d-7705-98ed-4ea63108e87e"
    }

    assignments, warnings = @rz.send(:assign_agent_sessions, rows, [codex, amp])

    assert_empty assignments
    assert_equal 2, warnings.length
    assert(warnings.all? { |warning| warning.include?("multiple agent types share this directory") })
  end

  def test_assigns_a_session_by_unique_project_title_when_ghostty_cwd_is_blank
    rows = [
      {
        "terminal_id" => "terminal-fws-docs",
        "terminal_name" => "fws-docs",
        "working_directory" => ""
      }
    ]
    session = {
      "agent" => "codex",
      "pid" => 18_717,
      "tty" => "ttys002",
      "cwd" => "/Users/marian/work/fws-docs",
      "session_id" => "019f7eb7-dc72-75b3-b042-91599cdd90ac"
    }

    assignments, warnings = @rz.send(:assign_agent_sessions, rows, [session])

    assert_equal session, assignments.fetch("terminal-fws-docs")
    assert_empty warnings
  end

  def test_does_not_guess_when_multiple_blank_terminals_share_a_project_title
    rows = [
      {
        "terminal_id" => "terminal-1",
        "terminal_name" => "fws-docs",
        "working_directory" => ""
      },
      {
        "terminal_id" => "terminal-2",
        "terminal_name" => "fws-docs",
        "working_directory" => ""
      }
    ]
    session = {
      "agent" => "codex",
      "pid" => 18_717,
      "tty" => "ttys002",
      "cwd" => "/Users/marian/work/fws-docs",
      "session_id" => "019f7eb7-dc72-75b3-b042-91599cdd90ac"
    }

    assignments, warnings = @rz.send(:assign_agent_sessions, rows, [session])

    assert_empty assignments
    assert_equal 1, warnings.length
    assert_includes warnings.first, session.fetch("session_id")
    assert_includes warnings.first, session.fetch("tty")
    assert_includes warnings.first, "multiple blank-directory Ghostty terminals"
  end

  def test_window_scoped_matching_does_not_warn_about_unrelated_sessions
    session = {
      "agent" => "codex",
      "pid" => 18_927,
      "tty" => "ttys006",
      "cwd" => "/Users/marian/work/playground/ark-mp-com",
      "session_id" => "019f7a33-7f86-7450-ac59-4dde828fa26d"
    }

    assignments, warnings = @rz.send(
      :assign_agent_sessions,
      [],
      [session],
      warn_for_all_sessions: false
    )

    assert_empty assignments
    assert_empty warnings
  end

  def test_assigns_a_session_using_its_unique_title_from_a_previous_snapshot
    snapshot_dir = File.join(@state_dir, "snapshots", "work_20260720-173021")
    FileUtils.mkdir_p(snapshot_dir)
    File.write(
      File.join(snapshot_dir, "state.json"),
      JSON.generate(
        "windows" => [
          {
            "tabs" => [
              {
                "terminals" => [
                  {
                    "name" => "cronos_dawn_opt",
                    "working_directory" => "/Users/marian/work/playground/ark-mp-com",
                    "codex_session_id" => "019f7a33-7f86-7450-ac59-4dde828fa26d"
                  }
                ]
              }
            ]
          }
        ]
      )
    )
    rows = [
      {
        "terminal_id" => "terminal-cronos",
        "terminal_name" => "⠹ cronos_dawn_opt",
        "working_directory" => ""
      }
    ]
    session = {
      "agent" => "codex",
      "pid" => 18_927,
      "tty" => "ttys006",
      "cwd" => "/Users/marian/work/playground/ark-mp-com",
      "session_id" => "019f7a33-7f86-7450-ac59-4dde828fa26d"
    }

    assignments, warnings = @rz.send(:assign_agent_sessions, rows, [session])

    assert_equal session, assignments.fetch("terminal-cronos")
    assert_empty warnings
  end

  def test_codex_process_must_descend_from_ghostty_when_ghostty_is_detected
    ghostty = {
      "pid" => 100,
      "ppid" => 1,
      "tty" => "??",
      "command" => "/Applications/Ghostty.app/Contents/MacOS/ghostty"
    }
    shell = {
      "pid" => 200,
      "ppid" => 100,
      "tty" => "ttys002",
      "command" => "/bin/zsh -l"
    }
    codex = {
      "pid" => 300,
      "ppid" => 200,
      "tty" => "ttys002",
      "command" => "/opt/homebrew/bin/codex"
    }
    process_by_pid = [ghostty, shell, codex].to_h { |process| [process.fetch("pid"), process] }

    assert @rz.send(:descendant_of?, codex, [ghostty.fetch("pid")], process_by_pid)

    external_codex = codex.merge("pid" => 400, "ppid" => 1, "tty" => "ttys009")
    refute @rz.send(:descendant_of?, external_codex, [ghostty.fetch("pid")], process_by_pid)
  end

  def test_restore_reports_the_working_directory_before_resuming_codex
    terminal = {
      "working_directory" => "/Users/marian/work/fws-docs",
      "codex_session_id" => "019f7eb7-dc72-75b3-b042-91599cdd90ac"
    }

    command = @rz.send(:restore_command, terminal, @state_dir, [], [])
    shell_command = Shellwords.split(command).last

    assert_includes shell_command, "kitty-shell-cwd://%s%s"
    assert_includes shell_command, terminal.fetch("working_directory")
    assert_includes shell_command, "exec codex resume"
  end

  def test_restore_continues_the_saved_amp_thread
    thread_id = "T-01a023e2-3f9d-7705-98ed-4ea63108e87e"
    terminal = {
      "working_directory" => "/Users/marian/dot-files",
      "amp_thread_id" => thread_id
    }
    duplicates = []

    command = @rz.send(:restore_command, terminal, @state_dir, [], duplicates)
    shell_command = Shellwords.split(command).last

    assert_includes shell_command, "exec amp threads continue #{thread_id}"
    assert_empty duplicates

    command = @rz.send(:restore_command, terminal, @state_dir, [["amp", thread_id]], duplicates)
    shell_command = Shellwords.split(command).last

    assert_includes shell_command, "Amp thread already running"
    refute_includes shell_command, "exec amp threads continue"
    assert_equal [{"agent" => "amp", "session_id" => thread_id}], duplicates
  end

  def test_restore_applescript_closes_only_windows_present_before_restore
    snapshot = {
      "windows" => [
        {
          "tabs" => [
            {
              "name" => "Work",
              "selected" => true,
              "terminals" => [
                {
                  "focused" => true,
                  "working_directory" => "/tmp/project"
                }
              ]
            }
          ]
        }
      ]
    }

    script, = @rz.send(
      :restore_applescript,
      snapshot,
      @state_dir,
      [],
      close_existing: true,
      ready_file: "/tmp/ghostty-rz-test.ready"
    )

    assert_includes script, "set rzExistingWindowIds to id of every window"
    assert_includes script, "set rzWindow1 to new window"
    assert_includes script, "/usr/bin/nohup /usr/bin/osascript"
    assert_includes script, 'quote & (rzExistingWindowId as text) & quote'
    assert_includes script, 'perform action \\"close_window\\" on rzTerminalToClose'
    assert_includes script, 'static text \\"Close Window?\\"'
    assert_includes script, 'click button \\"Close\\" of sheet 1 of rzUiWindow'
    assert_includes script, "/usr/bin/touch -- /tmp/ghostty-rz-test.ready"
    assert_operator script.index("set rzExistingWindowIds"), :<, script.index("set rzWindow1")
    assert_operator script.index("set rzWindow1"), :<, script.index("set rzCloseScript")

    command = @rz.send(
      :restore_command,
      {"working_directory" => "/tmp/project"},
      @state_dir,
      [],
      [],
      ready_file: "/tmp/ghostty-rz-test.ready"
    )
    shell_command = Shellwords.split(command).last
    assert_includes shell_command, "until [ -e /tmp/ghostty-rz-test.ready ]"
  end

  def test_restore_applescript_can_leave_existing_windows_open
    snapshot = {
      "windows" => [
        {
          "tabs" => [
            {
              "name" => "Work",
              "selected" => true,
              "terminals" => [
                {
                  "focused" => true,
                  "working_directory" => "/tmp/project"
                }
              ]
            }
          ]
        }
      ]
    }

    script, = @rz.send(
      :restore_applescript,
      snapshot,
      @state_dir,
      [],
      close_existing: false
    )

    refute_includes script, "rzExistingWindowIds"
    refute_includes script, "rzCloseScript"
  end
end
